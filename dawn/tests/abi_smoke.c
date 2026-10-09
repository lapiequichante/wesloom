/* Dynamic-loading acceptance test; also compiled as C++ through abi_smoke.cpp. */
#include "wxsl.h"
#include "wxsl_host.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <windows.h>
#else
#include <dlfcn.h>
#endif

#define REQUIRE(condition) do { if (!(condition)) { fprintf(stderr, "%s:%d: %s\n", __FILE__, __LINE__, #condition); exit(1); } } while (0)
#define FUNCTIONS(X) \
    X(wxsl_abi_version) X(wxsl_compile_pipeline) X(wxsl_compile_shader) \
    X(wxsl_compile_material) X(wxsl_check_setup) X(wxsl_result_status) \
    X(wxsl_result_json) X(wxsl_result_wgsl) X(wxsl_result_params) \
    X(wxsl_result_field_count) X(wxsl_result_fields) X(wxsl_result_free)
#define DECLARE(name) static wxsl_fn_##name api_##name;
FUNCTIONS(DECLARE)

static WxslBytes bytes(const char *text) {
    WxslBytes view = {(const uint8_t *)text, strlen(text)};
    return view;
}

static int contains(WxslBytes view, const char *needle) {
    size_t len = strlen(needle);
    size_t i;
    for (i = 0; len <= view.len && i <= view.len - len; ++i) {
        if (memcmp(view.data + i, needle, len) == 0) return 1;
    }
    return 0;
}

static char *read_file(const char *directory, const char *name, size_t *length) {
    char path[4096];
    FILE *file;
    long size;
    char *data;
    int written = snprintf(path, sizeof(path), "%s/%s", directory, name);
    REQUIRE(written > 0 && (size_t)written < sizeof(path));
    file = fopen(path, "rb");
    REQUIRE(file != NULL);
    REQUIRE(fseek(file, 0, SEEK_END) == 0);
    size = ftell(file);
    REQUIRE(size >= 0);
    rewind(file);
    *length = (size_t)size;
    data = (char *)malloc(*length + 1);
    REQUIRE(data != NULL);
    REQUIRE(fread(data, 1, *length, file) == *length);
    data[*length] = '\0';
    REQUIRE(fclose(file) == 0);
    return data;
}

static void expect(WxslResult *result, WxslStatus status) {
    REQUIRE(result != NULL);
    if (api_wxsl_result_status(result) != status) {
        WxslBytes diagnostic = api_wxsl_result_json(result);
        fwrite(diagnostic.data, 1, diagnostic.len, stderr);
        fputc('\n', stderr);
        exit(1);
    }
}

int main(int argc, char **argv) {
    const char *presets[] = {"forward.pipeline.json", "deferred.pipeline.json"};
    const char *stages[] = {"forward_lit", "gbuffer", "depth_only", "shadow", "velocity", "peel_front", "peel_back", "peel_depth", "peel_resolve"};
    size_t i;
    REQUIRE(argc == 4);
#ifdef _WIN32
    HMODULE library = LoadLibraryA(argv[1]);
    REQUIRE(library != NULL);
#define LOAD(name) do { FARPROC symbol = GetProcAddress(library, #name); REQUIRE(symbol != NULL); REQUIRE(sizeof(symbol) == sizeof(api_##name)); memcpy(&api_##name, &symbol, sizeof(symbol)); } while (0);
#else
    void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (library == NULL) { fprintf(stderr, "%s\n", dlerror()); return 1; }
#define LOAD(name) do { void *symbol = dlsym(library, #name); REQUIRE(symbol != NULL); REQUIRE(sizeof(symbol) == sizeof(api_##name)); memcpy(&api_##name, &symbol, sizeof(symbol)); } while (0);
#endif
    FUNCTIONS(LOAD)
    REQUIRE(api_wxsl_abi_version() == WXSL_ABI_VERSION);
    for (i = 0; i < sizeof(presets) / sizeof(presets[0]); ++i) {
        size_t length;
        char *document = read_file(argv[2], presets[i], &length);
        char *request = (char *)malloc(length + 32);
        WxslResult *result;
        REQUIRE(request != NULL);
        REQUIRE(snprintf(request, length + 32, "{\"pipeline\":%s}", document) > 0);
        result = api_wxsl_compile_pipeline(WXSL_ABI_VERSION, bytes(request));
        free(request);
        free(document); /* Result views must outlive the borrowed input. */
        expect(result, WXSL_STATUS_SUCCESS);
        REQUIRE(contains(api_wxsl_result_json(result), "\"schedule\""));
        REQUIRE(contains(api_wxsl_result_json(result), "\"allocations\""));
        REQUIRE(contains(api_wxsl_result_json(result), "\"order\":[0,1,2,3,4,5,6]"));
        REQUIRE(contains(api_wxsl_result_json(result), "wxsl.tonemap"));
        api_wxsl_result_free(result);
    }
    for (i = 0; i < sizeof(stages) / sizeof(stages[0]); ++i) {
        char name[128];
        size_t request_len, expected_len;
        char *request, *expected;
        WxslBytes wgsl;
        WxslResult *result;
        REQUIRE(snprintf(name, sizeof(name), "%s.request.json", stages[i]) > 0);
        request = read_file(argv[3], name, &request_len);
        result = api_wxsl_compile_material(WXSL_ABI_VERSION, bytes(request));
        free(request);
        expect(result, WXSL_STATUS_SUCCESS);
        REQUIRE(snprintf(name, sizeof(name), "%s.wgsl", stages[i]) > 0);
        expected = read_file(argv[3], name, &expected_len);
        wgsl = api_wxsl_result_wgsl(result);
        REQUIRE(wgsl.len == expected_len && memcmp(wgsl.data, expected, expected_len) == 0);
        free(expected);
        api_wxsl_result_free(result);
    }
    {
        WxslResult *result = api_wxsl_compile_shader(WXSL_ABI_VERSION, bytes("{\"effect\":\"bloom\"}"));
        const WxslField *fields;
        size_t count;
        expect(result, WXSL_STATUS_SUCCESS);
        count = api_wxsl_result_field_count(result);
        fields = api_wxsl_result_fields(result);
        REQUIRE(count == 3 && fields != NULL);
        REQUIRE(api_wxsl_result_params(result).len == fields[0].buffer_size);
        for (i = 0; i < count; ++i) {
            REQUIRE(fields[i].group == 3 && fields[i].binding == 1);
            REQUIRE(fields[i].offset % fields[i].align == 0);
            REQUIRE(fields[i].size == 4 && fields[i].buffer_align == 16);
            REQUIRE(fields[i].name.len > 0 && contains(fields[i].ty, "f32"));
        }
        REQUIRE(contains(api_wxsl_result_wgsl(result), "bloom_fs"));
        api_wxsl_result_free(result);
    }
    {
        WxslBytes invalid = {NULL, (size_t)-1};
        WxslResult *result = api_wxsl_compile_pipeline(WXSL_ABI_VERSION + 1, invalid);
        expect(result, WXSL_STATUS_ABI_MISMATCH);
        REQUIRE(contains(api_wxsl_result_json(result), "C ABI version"));
        api_wxsl_result_free(result);
        result = api_wxsl_compile_pipeline(WXSL_ABI_VERSION, bytes("{\"pipeline\":{\"abi\":999}}"));
        expect(result, WXSL_STATUS_INVALID_DOCUMENT);
        REQUIRE(contains(api_wxsl_result_json(result), "ABI revision 999"));
        api_wxsl_result_free(result);
        REQUIRE(api_wxsl_result_status(NULL) == WXSL_STATUS_INVALID_ARGUMENT);
        api_wxsl_result_free(NULL);
    }
    {
        const char *request = "{\"pipeline\":{\"domain\":\"document\",\"nodes\":[],\"edges\":[]},\"scene\":{\"materials\":[{\"name\":\"bad\",\"model\":\"unavailable\",\"graph\":{\"nodes\":[{\"id\":1,\"def\":\"output.surface\"}],\"edges\":[]}}]}}";
        WxslResult *result = api_wxsl_check_setup(WXSL_ABI_VERSION, bytes(request));
        expect(result, WXSL_STATUS_INCOMPATIBLE);
        REQUIRE(contains(api_wxsl_result_json(result), "unavailable"));
        api_wxsl_result_free(result);
    }
#ifdef _WIN32
    REQUIRE(FreeLibrary(library) != 0);
#else
    REQUIRE(dlclose(library) == 0);
#endif
    puts("C ABI: presets, nine native WGSL variants, field tables, capability/version refusals passed");
    return 0;
}
