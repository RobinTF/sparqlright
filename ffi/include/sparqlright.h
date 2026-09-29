// C API of SPARQLright for in-process fuzzing harnesses.
// See ffi/src/lib.rs for the documentation of each function.
#ifndef SPARQLRIGHT_H
#define SPARQLRIGHT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct Sparqlright Sparqlright;

// Runs a SELECT query and returns SPARQL JSON results in *json / *len
// (valid until the next call). Returns 0 on success.
typedef int (*sparqlright_select_callback)(void* context, const char* query,
                                           const char** json, size_t* len);

// `config` is a TOML generator configuration or NULL for the default.
Sparqlright* sparqlright_new_with_data(const char* rdf_file, const char* config);
Sparqlright* sparqlright_new_with_fixed_vocabulary(const char* config);
Sparqlright* sparqlright_new_with_engine(sparqlright_select_callback callback,
                                         void* context, const char* config);

// Turns fuzzer input into a query (update != 0: an update request).
// The result is owned by the generator and valid until the next call.
const char* sparqlright_generate(Sparqlright* generator, const uint8_t* data,
                                 size_t len, int update);

void sparqlright_free(Sparqlright* generator);

#ifdef __cplusplus
}
#endif

#endif  // SPARQLRIGHT_H
