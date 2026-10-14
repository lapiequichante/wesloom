#pragma once
#include <cstdint>
#include <filesystem>
#include <memory>
#include <nlohmann/json.hpp>
#include <string>
#include <vector>

namespace wxsl {
/// Which C ABI call a request goes to: `wxsl_compile_pipeline`,
/// `wxsl_compile_shader`, `wxsl_compile_material` or `wxsl_check_setup`.
enum class Operation { Pipeline, Shader, Material, Check };

/// A successful result, copied out of the foreign result before it is freed.
struct Compiled {
  /// `metadata` is the JSON envelope (`{version, abi, status, data}`);
  /// `fields` is the computed layout, one object per `WxslField` row.
  nlohmann::json metadata, fields;
  /// WGSL, empty unless the call compiled a shader.
  std::string wgsl;
  /// Initial parameter bytes, already padded to the computed layout.
  std::vector<uint8_t> params;
};

/// The runtime compiler: `wxsl-ffi` loaded dynamically, never linked (ADR 0052).
///
/// Holds a cache of up to 64 successful requests and is single-owner;
/// use one instance per worker thread. Every method throws
/// `std::runtime_error` on failure, with the library's JSON diagnostics in the
/// message.
class Compiler {
public:
  /// Load the `wxsl_ffi` shared library at `library` and check its ABI
  /// version against `WXSL_ABI_VERSION`.
  explicit Compiler(const std::filesystem::path &library);
  ~Compiler();
  Compiler(const Compiler &) = delete;
  Compiler &operator=(const Compiler &) = delete;
  /// Run one request (shapes in docs/cpp.md), or return the cached result
  /// for an identical one.
  std::shared_ptr<const Compiled> compile(Operation operation, const nlohmann::json &request);
  /// Requests that reached the library.
  uint64_t compilations() const;
  /// Requests answered from the cache.
  uint64_t cache_hits() const;

private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};
} // namespace wxsl
