#include "compiler.hpp"
#include "parity.hpp"
#include "renderer.hpp"
#include <chrono>
#include <fstream>
#include <iostream>
#include <stdexcept>

namespace {
void require(bool condition, const char *message) {
  if (!condition)
    throw std::runtime_error(message);
}
template <class F> void refuses(F call, const std::string &named) {
  try {
    call();
  } catch (const std::exception &error) {
    require(std::string(error.what()).find(named) != std::string::npos,
            "refusal lacked expected name");
    return;
  }
  throw std::runtime_error("invalid request was accepted: " + named);
}
struct Fixture {
  std::filesystem::path path;
  explicit Fixture(const std::filesystem::path &source) {
    path = std::filesystem::temp_directory_path() /
           ("wxsl-runtime-" +
            std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
    require(std::filesystem::create_directory(path), "fixture already exists");
    std::filesystem::copy(source, path, std::filesystem::copy_options::recursive);
    // Only our disposable copy is altered: runtime must not fall back to WGSL
    // files.
    for (const auto &file : std::filesystem::directory_iterator(path))
      if (file.path().extension() == ".wgsl")
        std::filesystem::remove(file.path());
  }
  ~Fixture() {
    std::error_code error;
    std::filesystem::remove_all(path, error);
  }
};
} // namespace
int main(int argc, char **argv) {
  try {
    require(argc == 3, "runtime_smoke needs a bundle and the compiler cdylib");
    wxsl::Renderer offline(argv[1]);
    const auto forward = offline.render("forward"), deferred = offline.render("deferred");
    Fixture fixture(argv[1]);
    std::shared_ptr<const wxsl::Compiled> copied;
    {
      wxsl::Compiler temporary(argv[2]);
      copied = temporary.compile(
          wxsl::Operation::Shader,
          {{"root", "test"},
           {"library", {{"modules", {{"test", "fn value() -> f32 { return 0.5; }"}}}}}});
    }
    require(!copied->wgsl.empty() && copied->metadata.at("data").at("root") == "test",
            "copied result did not survive library unload");
    wxsl::Compiler compiler(argv[2]);
    wxsl::Renderer runtime(fixture.path, &compiler);
    require(wxsl::compare_rgba(runtime.render("forward"), forward.data()).passes(),
            "runtime forward differs");
    require(wxsl::compare_rgba(runtime.render("deferred"), deferred.data()).passes(),
            "runtime deferred differs");
    nlohmann::json manifest;
    std::ifstream(fixture.path / "manifest.json") >> manifest;
    auto original = manifest.at("materials").at(0).at("stages").at("forward_lit").at("request");
    const auto before = compiler.compilations();
    const auto pipelines = runtime.pipeline_count();
    runtime.compile_material(compiler, 0, original);
    runtime.compile_material(compiler, 0, original);
    require(compiler.compilations() == before && compiler.cache_hits() >= 18,
            "identical material missed cache");
    require(runtime.pipeline_count() == pipelines, "identical material invalidated pipelines");
    const auto params = compiler.compile(wxsl::Operation::Material, original)->params;
    runtime.upload_material_params(0, params);
    require(compiler.compilations() == before && runtime.pipeline_count() == pipelines,
            "parameter write compiled");
    auto edited = original;
    edited["material"]["macros"]["wxsl_fbm_ridged"] = {{"flag", true}};
    runtime.compile_material(compiler, 0, edited);
    const auto changed = runtime.render("forward");
    require(changed != forward, "material edit changed no pixels");
    require(compiler.compile(wxsl::Operation::Material, edited)
                    ->metadata.at("data")
                    .at("variant_key") != compiler.compile(wxsl::Operation::Material, original)
                                              ->metadata.at("data")
                                              .at("variant_key"),
            "Rust variant key did not change");
    auto invalid = edited;
    invalid["graph"]["nodes"][0]["def"] = "missing.node";
    refuses([&] { runtime.compile_material(compiler, 0, invalid); }, "missing.node");
    require(runtime.render("forward") == changed, "compiler failure lost last valid material");
    auto invalid_shader = edited;
    invalid_shader["library"]["modules"]["package::generative::hash13"] =
        "fn hash13(p: vec3f) -> f32 { return vec3f(0.0); }";
    refuses([&] { runtime.compile_material(compiler, 0, invalid_shader); }, "Dawn:");
    require(runtime.render("forward") == changed,
            "GPU validation failure lost last valid material");
    auto layout = edited;
    bool found = false;
    for (auto &node : layout["graph"]["nodes"])
      if (node["def"] == "param.value") {
        node["settings"]["name"] = "renamed_parameter";
        found = true;
        break;
      }
    require(found, "runtime fixture needs a parameter");
    refuses([&] { runtime.compile_material(compiler, 0, layout); }, "layout changed");
    require(runtime.render("forward") == changed, "layout refusal lost last valid material");
    invalid = original;
    invalid["abi"] = 99;
    refuses([&] { compiler.compile(wxsl::Operation::Material, invalid); }, "abi 99");
    invalid = original;
    invalid["material"]["cast_shadow"] = false;
    refuses([&] { runtime.compile_material(compiler, 0, invalid); }, "draw selection");
    runtime.compile_material(compiler, 0, original);
    require(runtime.render("forward") == forward, "restoring original did not restore pixels");
    std::cout << "runtime shaders (no WGSL files), edits, cache, uploads and "
                 "transactional refusals passed\n";
    return 0;
  } catch (const std::exception &error) {
    std::cerr << error.what() << '\n';
    return 1;
  }
}
