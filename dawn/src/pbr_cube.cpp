#include "compiler.hpp"
#include "parity.hpp"
#include "renderer.hpp"
#define STB_IMAGE_IMPLEMENTATION
#define STB_IMAGE_WRITE_IMPLEMENTATION
#include <algorithm>
#include <cmath>
#include <iostream>
#include <limits>
#include <stb_image.h>
#include <stb_image_write.h>
#include <stdexcept>

int main(int argc, char **argv) {
  try {
    std::filesystem::path assets = "target/dawn-assets/pbr", output = "target/dawn-images";
    bool verify = false;
    std::filesystem::path compiler_library;
    std::string selected;
    std::filesystem::path hdri;
    std::string hdr_label = "source HDR";
    uint32_t frames = 1;
    for (int i = 1; i < argc; ++i) {
      const std::string arg = argv[i];
      auto value = [&]() {
        if (++i >= argc)
          throw std::runtime_error("missing value for " + arg);
        return std::string(argv[i]);
      };
      if (arg == "--headless")
        continue;
      if (arg == "--assets")
        assets = value();
      else if (arg == "--out")
        output = value();
      else if (arg == "--verify")
        verify = true;
      else if (arg == "--compiler")
        compiler_library = value();
      else if (arg == "--pipeline")
        selected = value();
      else if (arg == "--hdri")
        hdri = value();
      else if (arg == "--hdr-label")
        hdr_label = value();
      else if (arg == "--frames") {
        const auto input = value();
        size_t end = 0;
        const auto count = std::stoull(input, &end);
        if (end != input.size() || count == 0 || count > std::numeric_limits<uint32_t>::max())
          throw std::runtime_error("--frames must be a nonzero uint32");
        frames = static_cast<uint32_t>(count);
      } else
        throw std::runtime_error("unknown argument " + arg);
    }
    std::unique_ptr<wxsl::Compiler> compiler;
    if (!compiler_library.empty())
      compiler = std::make_unique<wxsl::Compiler>(compiler_library);
    wxsl::Renderer renderer(assets, compiler.get());
    if (!hdri.empty()) {
      if (!stbi_is_hdr(hdri.string().c_str()))
        throw std::runtime_error("--hdri expects a Radiance HDR file");
      int w, h, channels;
      std::unique_ptr<float, decltype(&stbi_image_free)> pixels(
          stbi_loadf(hdri.string().c_str(), &w, &h, &channels, 3), &stbi_image_free);
      if (!pixels) throw std::runtime_error("cannot decode HDR: " + hdri.string());
      const std::vector<float> rgb(pixels.get(), pixels.get() + size_t(w) * h * 3);
      renderer.set_environment_scale(renderer.upload_environment_image(hdr_label, w, h, rgb));
    }
    std::cout << renderer.capabilities().dump(2) << '\n';
    std::filesystem::create_directories(output);
    const std::vector<std::string> pipelines = selected.empty()
                                                   ? std::vector<std::string>{"forward", "deferred"}
                                                   : std::vector<std::string>{selected};
    for (const std::string &pipeline : pipelines) {
      auto pixels = renderer.render(pipeline, frames);
      const auto file = output / ("pbr_cube_" + pipeline + ".png");
      if (!stbi_write_png(file.string().c_str(), static_cast<int>(renderer.width()),
                          static_cast<int>(renderer.height()), 4, pixels.data(),
                          static_cast<int>(renderer.width() * 4)))
        throw std::runtime_error("PNG write failed");
      std::cout << pipeline << " -> " << file << '\n';
      if (verify) {
        const auto reference = assets / ("reference_" + pipeline + ".png");
        int width, height, channels;
        auto *expected = stbi_load(reference.string().c_str(), &width, &height, &channels, 4);
        if (!expected)
          throw std::runtime_error("cannot load Rust reference: " + reference.string());
        if (width != static_cast<int>(renderer.width()) ||
            height != static_cast<int>(renderer.height())) {
          stbi_image_free(expected);
          throw std::runtime_error("reference dimensions differ");
        }
        const auto parity = wxsl::compare_rgba(pixels, expected);
        stbi_image_free(expected);
        std::cout << "Dawn/Rust mean=" << parity.mean << ", outlier pixels=" << parity.outliers
                  << '\n';
        if (!parity.passes())
          throw std::runtime_error("backend parity failed for " + pipeline);
      }
    }
    return 0;
  } catch (const std::exception &error) {
    std::cerr << error.what() << '\n';
    return 1;
  }
}
