from conan import ConanFile
from conan.tools.cmake import cmake_layout


class WxslDawn(ConanFile):
    settings = "os", "arch", "compiler", "build_type"
    generators = "CMakeDeps", "CMakeToolchain"

    def requirements(self):
        self.requires("dawn-prebuilt/20261008")
        self.requires("nlohmann_json/3.12.0")
        self.requires("stb/cci.20240531")

    def layout(self):
        cmake_layout(self)
