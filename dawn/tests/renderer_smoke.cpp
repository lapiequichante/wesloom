#include "parity.hpp"
#include "renderer.hpp"
#define STB_IMAGE_IMPLEMENTATION
#include <chrono>
#include <fstream>
#include <iostream>
#include <limits>
#include <stb_image.h>
#include <stdexcept>

void require(bool condition, const char *message) {
  if (!condition)
    throw std::runtime_error(message);
}
void verify(const wxsl::Renderer &renderer, const std::filesystem::path &root,
            const std::string &name, const std::vector<uint8_t> &pixels) {
  int w, h, channels;
  auto *expected =
      stbi_load((root / ("reference_" + name + ".png")).string().c_str(), &w, &h, &channels, 4);
  require(expected != nullptr, "missing Rust probe reference");
  const bool dimensions = w == int(renderer.width()) && h == int(renderer.height());
  const auto parity = dimensions ? wxsl::compare_rgba(pixels, expected) : wxsl::Parity{1, 1};
  stbi_image_free(expected);
  require(dimensions && parity.passes(), "device probe differs from Rust");
  std::cout << name << ": mean=" << parity.mean << ", outliers=" << parity.outliers << '\n';
}
struct Fixture {
  std::filesystem::path path;
  explicit Fixture(const std::filesystem::path &source) {
    path = std::filesystem::temp_directory_path() /
           ("wxsl-dawn-probe-" +
            std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
    require(std::filesystem::create_directory(path), "temporary fixture already exists");
    std::filesystem::copy(source, path,
                          std::filesystem::copy_options::recursive |
                              std::filesystem::copy_options::overwrite_existing);
  }
  ~Fixture() {
    std::error_code error;
    std::filesystem::remove_all(path, error);
  }
};
int main(int argc, char **argv) {
  try {
    require(argc == 2, "renderer_smoke needs the exported --probes bundle");
    const std::filesystem::path root = argv[1];
    wxsl::Renderer renderer(root);
    const auto first = renderer.render("buffer_probe");
    require(first == renderer.render("buffer_probe", 2), "stable buffer was lost between calls");
    require(renderer.pass_run_count("fill ramp") == 1, "once compute was recorded again");
    require(renderer.pass_run_count("show ramp") == 3, "screen pass did not run per frame");
    verify(renderer, root, "buffer_probe", first);
    const auto previous = renderer.render("history_probe");
    require(previous[0] == 0, "first history read sees this frame instead of "
                              "zero-initialized history");
    const auto second = renderer.render("history_probe");
    require(second[0] == 255, "history did not rotate to the previous frame");
    verify(renderer, root, "history_probe", renderer.render("history_probe"));
    verify(renderer, root, "indirect_probe", renderer.render("indirect_probe", 3));
    const auto mips = renderer.render("cube_mips_probe");
    require(mips == renderer.render("cube_mips_probe", 2), "cube mip bake did not remain stable");
    verify(renderer, root, "cube_mips_probe", mips);
    for (const auto *name : {"ibl_constant_probe", "ibl_directional_probe", "ibl_hotspot_probe"}) {
      const auto baked = renderer.render(name);
      require(baked == renderer.render(name, 2), "IBL bake did not remain stable");
      require(renderer.pass_run_count("probe specular 5/2") == 1, "IBL bake ran twice");
      verify(renderer, root, name, baked);
    }
    for (const auto *name : {"ibl_forward_probe", "ibl_deferred_probe"}) {
      const auto first = renderer.render(name);
      const auto later = renderer.render(name, 2);
      if (first != later || renderer.pass_run_count("lighting IBL specular 5/5") != 1)
        throw std::runtime_error("environment lighting bake did not persist");
      verify(renderer, root, name, first);
    }
    Fixture hdr_fixture(root);
    const auto hdr_file = hdr_fixture.path / "constant.hdr";
    {
      std::ofstream output(hdr_file, std::ios::binary);
      output << "#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n";
      const char pixel[] = {64, 96, char(128), char(131)};
      output.write(pixel, 4);
    }
    int hw, hh, hc;
    std::unique_ptr<float, decltype(&stbi_image_free)> hdr_pixels(
        stbi_loadf(hdr_file.string().c_str(), &hw, &hh, &hc, 3), &stbi_image_free);
    require(hdr_pixels && hw == 1 && hh == 1, "native HDR file decode failed");
    renderer.set_environment_scale(renderer.upload_environment_image("source HDR", hw, hh,
        {hdr_pixels.get(), hdr_pixels.get() + 3}));
    for (const auto *name : {"hdr_forward_probe", "hdr_deferred_probe"}) {
      const auto hdr = renderer.render(name);
      require(hdr == renderer.render(name, 2), "imported HDR bake is not stable");
      verify(renderer, root, name, hdr);
      const auto count = renderer.pass_run_count("document IBL specular 5/5");
      renderer.upload_environment_image("source HDR", 1, 1, {4, 0, 0});
      require(hdr != renderer.render(name), "replacing imported HDR did not change pixels");
      require(renderer.pass_run_count("document IBL specular 5/5") == count + 1, "import replacement did not rebake");
      renderer.upload_environment_image("source HDR", 1, 1, {2, 3, 4});
      require(hdr == renderer.render(name), "restoring imported HDR did not restore pixels");
    }

    for (const auto &rgb : {std::vector<float>{-1, 0, 0},
                           std::vector<float>{std::numeric_limits<float>::infinity(), 0, 0},
                           std::vector<float>{0, 0}}) {
      bool refused = false;
      try { renderer.upload_environment_image("source HDR", 1, 1, rgb); }
      catch (const std::exception &) { refused = true; }
      require(refused, "invalid HDR upload was accepted");
    }
    require(renderer.upload_environment_image("source HDR", 1, 1, {131072, 0, 0}) == 8.0f,
            "HDR scaling must preserve float16-exceeding radiance");
    renderer.upload_environment_image("source HDR", 1, 1, {2, 3, 4});

    Fixture fixture(root);
    nlohmann::json manifest;
    std::ifstream(fixture.path / "manifest.json") >> manifest;
    const auto original = manifest;
    manifest["host_layout"] = "deliberate mismatch";
    {
      std::ofstream output(fixture.path / "manifest.json");
      output << manifest;
    }
    bool rejected = false;
    try {
      wxsl::Renderer invalid(fixture.path);
    } catch (const std::exception &error) {
      rejected = std::string(error.what()).find("host layout mismatch") != std::string::npos;
    }
    require(rejected, "stale host header was not refused by name");

    // A deliberately wrong host upload must turn image agreement red.
    {
      std::ofstream output(fixture.path / "manifest.json");
      output << original;
    }
    // Drop one C++ recording instruction, leaving the shared plan untouched.
    auto skipped = original;
    bool dropped = false;
    auto &instructions = skipped["pipelines"]["forward"]["pass_data"];
    for (auto it = instructions.rbegin(); it != instructions.rend(); ++it)
      if (!it->at("draws").empty()) {
        (*it)["draws"] = nlohmann::json::array();
        dropped = true;
        break;
      }
    require(dropped, "negative recording fixture had no draw");
    {
      std::ofstream output(fixture.path / "manifest.json");
      output << skipped;
    }
    wxsl::Renderer omitted(fixture.path);
    const auto correct = renderer.render("forward");
    require(!wxsl::compare_rgba(omitted.render("forward"), correct.data()).passes(),
            "omitted C++ draw did not turn parity red");
    {
      std::ofstream output(fixture.path / "manifest.json");
      output << original;
    }
    const auto instances = fixture.path / original.at("frame").at("instances").get<std::string>();
    std::vector<char> zeros(std::filesystem::file_size(instances), 0);
    {
      std::ofstream output(instances, std::ios::binary | std::ios::trunc);
      output.write(zeros.data(), zeros.size());
    }
    wxsl::Renderer wrong(fixture.path);
    require(!wxsl::compare_rgba(wrong.render("forward"), correct.data()).passes(),
            "broken instance upload did not turn parity red");
    std::cout << "history, stable buffers, indirect draws, stale layout and "
                 "broken-upload and omitted-recording guards passed\n";
    return 0;
  } catch (const std::exception &error) {
    std::cerr << error.what() << '\n';
    return 1;
  }
}
