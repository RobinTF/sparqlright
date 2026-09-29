#!/usr/bin/env bash
# Builds the libFuzzer target `qlever-sparqlright-fuzzer`.
#
# usage: fuzz/qlever/build.sh QLEVER_SOURCE_DIR QLEVER_FUZZ_BUILD_DIR [OUTPUT]
#
# QLEVER_FUZZ_BUILD_DIR must be a QLever build directory configured with
#   -DCMAKE_C_COMPILER=clang -DCMAKE_CXX_COMPILER=clang++
#   -DCMAKE_C_FLAGS="-fsanitize=fuzzer-no-link,address -fno-omit-frame-pointer"
#   -DCMAKE_CXX_FLAGS="-fsanitize=fuzzer-no-link,address -fno-omit-frame-pointer"
# The harness is compiled and linked with the same commands QLever uses for
# its `LibQLeverExample` target, so no changes to QLever are needed.
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 QLEVER_SOURCE_DIR QLEVER_FUZZ_BUILD_DIR [OUTPUT]" >&2
  exit 1
fi
src=$(realpath "$1")
build=$(realpath "$2")
out=$(realpath -m "${3:-$build/qlever-sparqlright-fuzzer}")
here=$(cd "$(dirname "$0")" && pwd)
root=$(realpath "$here/../..")

echo "== building the SPARQLright C API"
cargo build --release -p sparqlright-ffi --manifest-path "$root/Cargo.toml"
ffi_lib="$root/target/release/libsparqlright_ffi.a"

echo "== building the QLever libraries"
ninja -C "$build" LibQLeverExample

example_src="$src/src/libqlever/LibQLeverExample.cpp"
example_obj="CMakeFiles/LibQLeverExample.dir/src/libqlever/LibQLeverExample.cpp.o"
harness_obj="$build/qlever_sparqlright_fuzzer.o"

# Ninja writes commands as `: && CMD && :`; extra arguments must go to CMD.
unwrap() { sed -E 's/^: && //; s/ && :$//'; }

echo "== compiling the harness"
compile=$(ninja -C "$build" -t commands "$example_obj" | tail -1 | unwrap)
compile=${compile//"$example_src"/"$here/qlever_fuzzer.cpp"}
compile=${compile//"-o $example_obj"/"-o $harness_obj"}
# Drop the dependency-file options of the original object.
compile=$(sed -E 's/ -MD -MT [^ ]+ -MF [^ ]+//' <<<"$compile")
(cd "$build" && eval "$compile -I$root/ffi/include")

echo "== linking"
link=$(ninja -C "$build" -t commands LibQLeverExample | tail -1 | unwrap)
link=${link//"$example_obj"/"$harness_obj"}
link=${link//"-o LibQLeverExample"/"-o $out"}
(cd "$build" && eval "$link -fsanitize=fuzzer $ffi_lib -lpthread -ldl -lm")
echo "== built $out"
