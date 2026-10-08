#include <type_traits>
#include "wxsl.h"
static_assert(std::is_standard_layout<WxslBytes>::value, "byte view must have a C layout");
static_assert(std::is_standard_layout<WxslField>::value, "field table must have a C layout");
#include "abi_smoke.c"
