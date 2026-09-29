// Coverage-guided fuzzing of QLever with SPARQLright (libFuzzer target).
//
// libFuzzer mutates byte strings; SPARQLright turns each of them into a valid
// SPARQL query (the bytes are the generator's random choices), and the query
// is run in-process through libqlever. Coverage of QLever guides libFuzzer
// towards inputs, and therefore queries, that reach new code.
//
// An input is reported as a crash (so that libFuzzer saves it) if QLever
// reports an internal error: an exception whose message asks to "report this
// to the developers" (failed `AD_CORRECTNESS_CHECK`s and similar), or a
// `std::logic_error` such as `std::out_of_range`. Ordinary errors (invalid or
// unsupported queries, timeouts, memory limits) are not bugs.
//
// Configuration (environment variables):
//   SPARQLRIGHT_INDEX       basename of a QLever index (required)
//   SPARQLRIGHT_PROVIDER    where values come from: `data:FILE` (default if
//                           SPARQLRIGHT_DATA is set), `engine` (autocompletion
//                           queries against the same in-process QLever) or
//                           `fixed` (a built-in vocabulary)
//   SPARQLRIGHT_DATA        the RDF file of the index (for `data:`)
//   SPARQLRIGHT_CONFIG      a generator configuration (TOML file)
//   SPARQLRIGHT_TIMEOUT_MS  time limit per query (default 5000)
//   SPARQLRIGHT_IGNORE      `|`-separated message substrings that are known
//                           limitations and not reported
//   SPARQLRIGHT_VERBOSE     if set, print every query
//   SPARQLRIGHT_QUERY_FILE  if set, run the query in this file (instead of a
//                           generated one) through the same checks and exit;
//                           useful to check a reproducer

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <iostream>
#include <memory>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

#include "libqlever/Qlever.h"
#include "sparqlright.h"

namespace {

std::unique_ptr<qlever::Qlever> engine;
Sparqlright* generator = nullptr;
std::chrono::milliseconds timeLimit{5000};
std::vector<std::string> ignored;
bool verbose = false;

std::string env(const char* name, std::string fallback = "") {
  const char* value = std::getenv(name);
  return value ? std::string{value} : fallback;
}

std::vector<std::string> split(const std::string& text, char separator) {
  std::vector<std::string> parts;
  std::stringstream stream{text};
  for (std::string part; std::getline(stream, part, separator);) {
    if (!part.empty()) {
      parts.push_back(part);
    }
  }
  return parts;
}

bool isKnownLimitation(const std::string& message) {
  for (const auto& substring : ignored) {
    if (message.find(substring) != std::string::npos) {
      return true;
    }
  }
  return false;
}

// Runs `query` with the configured time limit and returns the SPARQL JSON
// result. Exceptions are passed on to the caller.
std::string run(const std::string& query) {
  auto planned = engine->parseAndPlanQuery(
      query, {}, std::make_shared<ad_utility::CancellationHandle<>>(),
      timeLimit);
  return engine->query(planned, ad_utility::MediaType::sparqlJson);
}

// The provider callback for `SPARQLRIGHT_PROVIDER=engine`. Failures of the
// provider's own queries are not reported; they only mean fewer suggestions.
int selectCallback(void*, const char* query, const char** json, size_t* len) {
  static std::string result;
  try {
    result = run(query);
  } catch (...) {
    return 1;
  }
  *json = result.data();
  *len = result.size();
  return 0;
}

[[noreturn]] void reportBug(const std::string& query, const std::string& kind,
                            const std::string& message) {
  std::cerr << "\n==== SPARQLright: QLever reported an internal error ("
            << kind << ")\n"
            << "---- message:\n"
            << message << "\n---- query:\n"
            << query << "\n====" << std::endl;
  std::abort();
}

std::string readFile(const std::string& path) {
  std::ifstream in{path};
  if (!in) {
    std::cerr << "cannot read " << path << std::endl;
    std::exit(1);
  }
  std::stringstream buffer;
  buffer << in.rdbuf();
  return buffer.str();
}

}  // namespace

void check(const std::string& query);

extern "C" int LLVMFuzzerInitialize(int*, char***) {
  auto index = env("SPARQLRIGHT_INDEX");
  if (index.empty()) {
    std::cerr << "set SPARQLRIGHT_INDEX to the basename of a QLever index"
              << std::endl;
    std::exit(1);
  }
  timeLimit = std::chrono::milliseconds{
      std::stoll(env("SPARQLRIGHT_TIMEOUT_MS", "5000"))};
  ignored = split(env("SPARQLRIGHT_IGNORE",
                      "Group graph pattern too large|countSubgraphs|"
                      "At most 64 triples"),
                  '|');
  verbose = !env("SPARQLRIGHT_VERBOSE").empty();

  qlever::EngineConfig config;
  config.baseName_ = index;
  config.persistUpdates_ = false;
  engine = std::make_unique<qlever::Qlever>(config);

  std::string configText;
  if (auto path = env("SPARQLRIGHT_CONFIG"); !path.empty()) {
    configText = readFile(path);
  }
  const char* configPtr = configText.empty() ? nullptr : configText.c_str();
  auto data = env("SPARQLRIGHT_DATA");
  auto provider = env("SPARQLRIGHT_PROVIDER", data.empty() ? "fixed" : "data:" + data);
  if (provider.rfind("data:", 0) == 0) {
    generator = sparqlright_new_with_data(provider.substr(5).c_str(), configPtr);
  } else if (provider == "engine") {
    generator = sparqlright_new_with_engine(selectCallback, nullptr, configPtr);
  } else if (provider == "fixed") {
    generator = sparqlright_new_with_fixed_vocabulary(configPtr);
  }
  if (generator == nullptr) {
    std::cerr << "cannot create the SPARQLright generator (provider "
              << provider << ")" << std::endl;
    std::exit(1);
  }
  if (auto path = env("SPARQLRIGHT_QUERY_FILE"); !path.empty()) {
    check(readFile(path));
    std::cerr << "SPARQLright: no internal error for " << path << std::endl;
    std::exit(0);
  }
  return 0;
}

// Runs `query` and reports internal errors of QLever as crashes.
void check(const std::string& query) {
  if (verbose) {
    std::cerr << "---- " << query << std::endl;
  }
  try {
    run(query);
  } catch (const std::logic_error& e) {
    if (!isKnownLimitation(e.what())) {
      reportBug(query, "std::logic_error", e.what());
    }
  } catch (const std::exception& e) {
    std::string message = e.what();
    if (message.find("Please report this to the developers") !=
            std::string::npos &&
        !isKnownLimitation(message)) {
      reportBug(query, "internal check", message);
    }
  }
}

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
  const char* text = sparqlright_generate(generator, data, size, 0);
  if (text == nullptr) {
    std::cerr << "SPARQLright failed to generate a query" << std::endl;
    std::abort();
  }
  check(text);
  return 0;
}
