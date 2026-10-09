#pragma once
#include <cmath>
#include <cstdint>
#include <vector>

namespace wxsl {
struct Parity {
  double mean, outliers;
  bool passes() const { return mean <= 0.005 && outliers <= 0.01; }
};
// Thresholds are fixed in ADR 0051, before the first Dawn render.
inline Parity compare_rgba(const std::vector<uint8_t> &actual, const uint8_t *expected) {
  double error = 0;
  size_t outliers = 0;
  for (size_t i = 0; i < actual.size(); i += 4) {
    bool outlier = false;
    for (size_t c = 0; c < 4; ++c) {
      const double delta = std::abs(int(actual[i + c]) - int(expected[i + c])) / 255.0;
      error += delta;
      if (c < 3 && delta > 0.05)
        outlier = true;
    }
    outliers += outlier;
  }
  return {error / double(actual.size()), double(outliers) / double(actual.size() / 4)};
}
} // namespace wxsl
