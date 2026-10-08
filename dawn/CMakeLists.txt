cmake_minimum_required(VERSION 3.16)
project(wxsl_dawn LANGUAGES C CXX)

# B2 only: no Dawn dependency until B3 implements the device half.
set(WXSL_FFI_LIBRARY "" CACHE FILEPATH "Built wxsl-ffi cdylib")
set(WXSL_ABI_FIXTURES "" CACHE PATH "Native Rust shader parity fixtures")
if(NOT EXISTS "${WXSL_FFI_LIBRARY}" OR NOT IS_DIRECTORY "${WXSL_ABI_FIXTURES}")
    message(FATAL_ERROR "Set WXSL_FFI_LIBRARY and WXSL_ABI_FIXTURES; see dawn/README.md")
endif()

enable_testing()
foreach(language c cpp)
    add_executable(abi_smoke_${language} tests/abi_smoke.${language})
    target_include_directories(abi_smoke_${language} PRIVATE ../crates/wxsl-ffi/include)
    target_link_libraries(abi_smoke_${language} PRIVATE ${CMAKE_DL_LIBS})
    if(MSVC)
        target_compile_options(abi_smoke_${language} PRIVATE /W4 /WX)
    else()
        target_compile_options(abi_smoke_${language} PRIVATE -Wall -Wextra -Wpedantic -Werror)
    endif()
    add_test(NAME abi_${language} COMMAND abi_smoke_${language}
        "${WXSL_FFI_LIBRARY}" "${CMAKE_CURRENT_SOURCE_DIR}/../crates/wxsl-frame/assets/presets"
        "${WXSL_ABI_FIXTURES}")
endforeach()
set_target_properties(abi_smoke_c PROPERTIES C_STANDARD 11 C_STANDARD_REQUIRED YES)
set_target_properties(abi_smoke_cpp PROPERTIES CXX_STANDARD 11 CXX_STANDARD_REQUIRED YES)
