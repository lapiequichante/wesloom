from conan import ConanFile
from conan.errors import ConanInvalidConfiguration
from conan.tools.files import copy, download, get


class DawnPrebuilt(ConanFile):
    name = "dawn-prebuilt"
    version = "20261008"
    package_type = "static-library"
    license = "BSD-3-Clause"
    url = "https://github.com/google/dawn"
    settings = "os", "arch", "build_type"
    commit = "b73d662f73d57bb42209546910666a38e6db95a1"
    release = "v20261008.181332"
    archives = {
        ("Macos", "armv8"): ("macos-latest", "c50c1a3e0239d0c07b584a35f6efb7d7a1e9daa1acacf9b762ae9cb3e34d50d7"),
        ("Macos", "x86_64"): ("macos-15-intel", "e649b4ff48b98cac4809d647bac8e64a2f7763ec99e8a0c1c8c1ae2591dbd1f5"),
        ("Linux", "x86_64"): ("ubuntu-latest", "4e72924d565c4a7bfecd9b3ad882c2cf8b53badb50f500e5e67457d4adbb32a3"),
        ("Windows", "x86_64"): ("windows-latest", "b3574efe866329252d738fbb8346e5c1547bdbc23a9db42debfecb267a90cc1e"),
    }

    def validate(self):
        if (str(self.settings.os), str(self.settings.arch)) not in self.archives:
            raise ConanInvalidConfiguration("No pinned official Dawn archive for this OS/architecture")
        if str(self.settings.build_type) != "Release":
            raise ConanInvalidConfiguration("The pinned Dawn package is Release-only")

    def build(self):
        platform, digest = self.archives[(str(self.settings.os), str(self.settings.arch))]
        archive = f"Dawn-{self.commit}-{platform}-Release.tar.gz"
        get(self, f"{self.url}/releases/download/{self.release}/{archive}", sha256=digest, strip_root=True)
        download(self, f"https://raw.githubusercontent.com/google/dawn/{self.commit}/LICENSE", "LICENSE")

    def package(self):
        copy(self, "LICENSE", src=self.build_folder, dst=f"{self.package_folder}/licenses")
        for folder in ["include", "lib"]:
            copy(self, "*", src=f"{self.build_folder}/{folder}", dst=f"{self.package_folder}/{folder}")

    def package_info(self):
        self.cpp_info.set_property("cmake_file_name", "Dawn")
        # Use the SDK's exported target and platform links, not another mirror.
        self.cpp_info.set_property("cmake_find_mode", "none")
        self.cpp_info.builddirs = ["lib/cmake/Dawn"]
        self.cpp_info.libs = ["webgpu_dawn"]
