//! C API of SPARQLright for in-process fuzzing harnesses.
//!
//! A harness (for example a libFuzzer target linked against an engine)
//! creates a generator once and then turns each fuzzer input into a SPARQL
//! request with [`sparqlright_generate`]. The fuzzer mutates the input bytes,
//! which are the generator's random choices, so every input yields a valid
//! request.
//!
//! The values in the requests come from a completion provider, as in the rest
//! of SPARQLright: an RDF file ([`sparqlright_new_with_data`]), a fixed
//! vocabulary ([`sparqlright_new_with_fixed_vocabulary`]), or the engine under
//! test itself through a callback ([`sparqlright_new_with_engine`]).
//!
//! The header is `include/sparqlright.h`.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use sparqlright::engine::{EngineError, Response, SparqlService};
use sparqlright::provider::{
    CompletionProvider, EngineProvider, EngineProviderConfig, FixedProvider, GraphProvider,
    MixProvider,
};
use sparqlright::{Generator, GeneratorConfig};

/// Runs a SELECT query in the engine and returns the results as SPARQL JSON.
///
/// On success, the callback stores a pointer to the JSON text in `*json` and
/// its length in `*len`, and returns 0. The text must stay valid until the
/// next call of the callback. Any other return value means failure.
pub type SelectCallback = unsafe extern "C" fn(
    context: *mut c_void,
    query: *const c_char,
    json: *mut *const c_char,
    len: *mut usize,
) -> c_int;

/// A generator with its provider.
pub struct Sparqlright {
    generator: Generator,
    provider: MixProvider,
    last: CString,
}

/// A [`SparqlService`] that calls back into the harness.
struct CallbackService {
    callback: SelectCallback,
    context: *mut c_void,
}

impl SparqlService for CallbackService {
    fn query(&self, query: &str, _accept: &str) -> Result<Response, EngineError> {
        let query = CString::new(query).map_err(|e| EngineError::Connection(e.to_string()))?;
        let mut json: *const c_char = std::ptr::null();
        let mut len = 0usize;
        // SAFETY: the harness promised in `sparqlright_new_with_engine` that
        // `callback` may be called with `context` and valid pointers.
        let status =
            unsafe { (self.callback)(self.context, query.as_ptr(), &raw mut json, &raw mut len) };
        if status != 0 || json.is_null() {
            return Err(EngineError::Connection(format!(
                "callback failed with {status}"
            )));
        }
        // SAFETY: on success the callback provides `len` readable bytes.
        let bytes = unsafe { std::slice::from_raw_parts(json.cast::<u8>(), len) };
        Ok(Response {
            status: 200,
            content_type: Some("application/sparql-results+json".to_owned()),
            body: String::from_utf8_lossy(bytes).into_owned(),
            elapsed: Duration::ZERO,
        })
    }

    fn update(&self, _update: &str) -> Result<Response, EngineError> {
        Err(EngineError::Connection(
            "updates are not supported by the callback".to_owned(),
        ))
    }
}

/// Reads an optional TOML configuration.
///
/// # Safety
///
/// `config` must be null or a valid NUL-terminated string.
unsafe fn config_from(config: *const c_char) -> Option<GeneratorConfig> {
    if config.is_null() {
        return Some(GeneratorConfig::default());
    }
    // SAFETY: guaranteed by the caller.
    let text = unsafe { CStr::from_ptr(config) }.to_str().ok()?;
    GeneratorConfig::from_toml(text).ok()
}

fn boxed(
    config: Option<GeneratorConfig>,
    terms: impl CompletionProvider + 'static,
) -> *mut Sparqlright {
    match config {
        Some(config) => Box::into_raw(Box::new(Sparqlright {
            generator: Generator::new(config),
            provider: MixProvider::variables_and(terms),
            last: CString::default(),
        })),
        None => std::ptr::null_mut(),
    }
}

/// Creates a generator whose values come from an RDF file (Turtle, TriG,
/// N-Triples or N-Quads), typically the data the engine was loaded with.
///
/// `config` is a generator configuration in TOML, or null for the default.
/// Returns null if the file or the configuration cannot be read.
///
/// # Safety
///
/// `path` must be a valid NUL-terminated string, `config` null or one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparqlright_new_with_data(
    path: *const c_char,
    config: *const c_char,
) -> *mut Sparqlright {
    catch_unwind(|| {
        // SAFETY: guaranteed by the caller.
        let path = unsafe { CStr::from_ptr(path) }.to_str().ok()?;
        let provider = GraphProvider::from_file(path).ok()?;
        // SAFETY: guaranteed by the caller.
        Some(boxed(unsafe { config_from(config) }, provider))
    })
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

/// Creates a generator with a small built-in vocabulary.
///
/// # Safety
///
/// `config` must be null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparqlright_new_with_fixed_vocabulary(
    config: *const c_char,
) -> *mut Sparqlright {
    // SAFETY: guaranteed by the caller.
    let config = unsafe { config_from(config) };
    catch_unwind(AssertUnwindSafe(|| boxed(config, FixedProvider::example())))
        .unwrap_or(std::ptr::null_mut())
}

/// Creates a generator whose values come from the engine under test: its
/// autocompletion queries are passed to `callback`.
///
/// # Safety
///
/// `callback` must be safe to call with `context` from the thread that calls
/// [`sparqlright_generate`], for as long as the generator exists. `config`
/// must be null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparqlright_new_with_engine(
    callback: SelectCallback,
    context: *mut c_void,
    config: *const c_char,
) -> *mut Sparqlright {
    // SAFETY: guaranteed by the caller.
    let config = unsafe { config_from(config) };
    let service = CallbackService { callback, context };
    catch_unwind(AssertUnwindSafe(|| {
        boxed(
            config,
            EngineProvider::new(service, EngineProviderConfig::default()),
        )
    }))
    .unwrap_or(std::ptr::null_mut())
}

/// Turns the fuzzer input `data` into a query (or, if `update` is non-zero,
/// an update request).
///
/// Returns a NUL-terminated string owned by the generator, valid until the
/// next call, or null if generation panicked.
///
/// # Safety
///
/// `generator` must come from one of the constructors and not be freed;
/// `data` must point to `len` readable bytes (or be null if `len` is 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparqlright_generate(
    generator: *mut Sparqlright,
    data: *const u8,
    len: usize,
    update: c_int,
) -> *const c_char {
    // SAFETY: guaranteed by the caller.
    let Some(s) = (unsafe { generator.as_mut() }) else {
        return std::ptr::null();
    };
    let bytes = if len == 0 || data.is_null() {
        &[][..]
    } else {
        // SAFETY: guaranteed by the caller.
        unsafe { std::slice::from_raw_parts(data, len) }
    };
    let text = catch_unwind(AssertUnwindSafe(|| {
        if update == 0 {
            s.generator.query_from_bytes(bytes, &mut s.provider).text
        } else {
            s.generator.update_from_bytes(bytes, &mut s.provider).text
        }
    }));
    match text.ok().and_then(|t| CString::new(t).ok()) {
        Some(c) => {
            s.last = c;
            s.last.as_ptr()
        }
        None => std::ptr::null(),
    }
}

/// Frees a generator.
///
/// # Safety
///
/// `generator` must come from one of the constructors (or be null) and must
/// not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sparqlright_free(generator: *mut Sparqlright) {
    if !generator.is_null() {
        // SAFETY: guaranteed by the caller.
        drop(unsafe { Box::from_raw(generator) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_from_bytes_through_the_c_api() {
        // SAFETY: null config is allowed.
        let g = unsafe { sparqlright_new_with_fixed_vocabulary(std::ptr::null()) };
        assert!(!g.is_null());
        let data = [7u8, 1, 200, 3, 99, 4, 5];
        // SAFETY: `g` is valid and `data` has `data.len()` bytes.
        let q = unsafe { sparqlright_generate(g, data.as_ptr(), data.len(), 0) };
        // SAFETY: the returned pointer is a valid C string.
        let text = unsafe { CStr::from_ptr(q) }.to_str().unwrap().to_owned();
        assert!(
            sparqlright::validate::validate_query(&text)
                .error()
                .is_none(),
            "{text}"
        );
        // SAFETY: `g` came from a constructor.
        unsafe { sparqlright_free(g) };
    }
}
