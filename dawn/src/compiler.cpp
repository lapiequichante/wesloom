#include "compiler.hpp"
#include "wxsl.h"
#include "wxsl_host.h"
#include <cstring>
#include <map>
#include <stdexcept>
#ifdef _WIN32
#include <windows.h>
#else
#include <dlfcn.h>
#endif

namespace wxsl {
namespace {
#define WXSL_FUNCTIONS(X)                                                                          \
  X(wxsl_abi_version)                                                                              \
  X(wxsl_compile_pipeline)                                                                         \
  X(wxsl_compile_shader)                                                                           \
  X(wxsl_compile_material) X(wxsl_check_setup) X(wxsl_result_status) X(wxsl_result_json)           \
      X(wxsl_result_wgsl) X(wxsl_result_params) X(wxsl_result_field_count) X(wxsl_result_fields)   \
          X(wxsl_result_free)
struct Library {
#ifdef _WIN32
  HMODULE handle;
  explicit Library(const std::filesystem::path &path) : handle(LoadLibraryW(path.c_str())) {
    if (!handle)
      throw std::runtime_error("cannot load compiler library: " + path.string());
  }
  ~Library() { FreeLibrary(handle); }
  auto symbol(const char *name) { return GetProcAddress(handle, name); }
#else
  void *handle;
  explicit Library(const std::filesystem::path &path)
      : handle(dlopen(path.c_str(), RTLD_NOW | RTLD_LOCAL)) {
    if (!handle)
      throw std::runtime_error("cannot load compiler library: " + std::string(dlerror()));
  }
  ~Library() { dlclose(handle); }
  auto symbol(const char *name) { return dlsym(handle, name); }
#endif
  Library(const Library &) = delete;
  Library &operator=(const Library &) = delete;
};
std::string copy(WxslBytes bytes) {
  return bytes.len ? std::string(reinterpret_cast<const char *>(bytes.data), bytes.len)
                   : std::string();
}
} // namespace
struct Compiler::Impl {
  Library library; // Outlives all function pointers and foreign results.
#define DECLARE(name) wxsl_fn_##name name;
  WXSL_FUNCTIONS(DECLARE)
#undef DECLARE
  std::map<std::string, std::shared_ptr<const Compiled>> cache;
  uint64_t compilations = 0, hits = 0;
  explicit Impl(const std::filesystem::path &path) : library(path) {
#define LOAD(name)                                                                                 \
  {                                                                                                \
    auto symbol = library.symbol(#name);                                                           \
    if (!symbol)                                                                                   \
      throw std::runtime_error("compiler library lacks " #name);                                   \
    static_assert(sizeof(symbol) == sizeof(name));                                                 \
    std::memcpy(&name, &symbol, sizeof(name));                                                     \
  }
    // Version is the only symbol called before checking the transport contract.
    LOAD(wxsl_abi_version)
    if (wxsl_abi_version() != WXSL_ABI_VERSION)
      throw std::runtime_error("compiler C ABI version mismatch");
    WXSL_FUNCTIONS(LOAD)
#undef LOAD
  }
  std::shared_ptr<const Compiled> compile(Operation operation, const nlohmann::json &request) {
    const auto input = request.dump();
    const auto key = std::to_string(static_cast<int>(operation)) + ":" + input;
    if (auto found = cache.find(key); found != cache.end()) {
      ++hits;
      return found->second;
    }
    wxsl_fn_wxsl_compile_material call;
    switch (operation) {
    case Operation::Pipeline:
      call = wxsl_compile_pipeline;
      break;
    case Operation::Shader:
      call = wxsl_compile_shader;
      break;
    case Operation::Material:
      call = wxsl_compile_material;
      break;
    case Operation::Check:
      call = wxsl_check_setup;
      break;
    default:
      throw std::runtime_error("unknown compiler operation");
    }
    ++compilations;
    const WxslBytes bytes{reinterpret_cast<const uint8_t *>(input.data()), input.size()};
    std::unique_ptr<WxslResult, wxsl_fn_wxsl_result_free> result(call(WXSL_ABI_VERSION, bytes),
                                                                 wxsl_result_free);
    if (!result)
      throw std::runtime_error("compiler returned a null result");
    auto output = std::make_shared<Compiled>();
    output->metadata = nlohmann::json::parse(copy(wxsl_result_json(result.get())));
    if (output->metadata.at("version") != WXSL_ABI_VERSION ||
        output->metadata.at("abi") != WXSL_SHADER_ABI_REVISION)
      throw std::runtime_error("compiler result version/ABI mismatch");
    if (wxsl_result_status(result.get()) != WXSL_STATUS_SUCCESS)
      throw std::runtime_error("WXSL compilation refused: " + output->metadata.at("data").dump());
    output->wgsl = copy(wxsl_result_wgsl(result.get()));
    const auto params = wxsl_result_params(result.get());
    if (params.len)
      output->params.assign(params.data, params.data + params.len);
    output->fields = nlohmann::json::array();
    const auto *fields = wxsl_result_fields(result.get());
    for (size_t i = 0; i < wxsl_result_field_count(result.get()); ++i) {
      const auto &field = fields[i];
      output->fields.push_back({{"name", copy(field.name)},
                                {"ty", copy(field.ty)},
                                {"buffer", field.buffer},
                                {"group", field.group},
                                {"binding", field.binding},
                                {"offset", field.offset},
                                {"size", field.size},
                                {"align", field.align},
                                {"buffer_size", field.buffer_size},
                                {"buffer_align", field.buffer_align}});
    }
    if (cache.size() == 64)
      cache.erase(cache.begin());
    cache.emplace(key, output);
    return output;
  }
};
Compiler::Compiler(const std::filesystem::path &library) : impl_(std::make_unique<Impl>(library)) {}
Compiler::~Compiler() = default;
std::shared_ptr<const Compiled> Compiler::compile(Operation operation,
                                                  const nlohmann::json &request) {
  return impl_->compile(operation, request);
}
uint64_t Compiler::compilations() const { return impl_->compilations; }
uint64_t Compiler::cache_hits() const { return impl_->hits; }
} // namespace wxsl
