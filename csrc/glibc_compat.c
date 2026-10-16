/*
 * Lets index01 link on glibc < 2.38 (Ubuntu 22.04, Debian 12).
 *
 * ort's prebuilt static ONNX Runtime was compiled against glibc 2.38+, whose C23 headers
 * redirect strtol/strtoll/strtoull/sscanf to __isoc23_* variants. The only difference is
 * that the C23 versions also accept a "0b" binary prefix, which ONNX Runtime never relies
 * on, so forwarding to the classic functions is safe. This file is compiled without C23
 * mode, so the calls below bind to the classic symbols. On newer glibc these definitions
 * simply take precedence over libc's equivalent ones.
 */
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>

long __isoc23_strtol(const char *s, char **end, int base) { return strtol(s, end, base); }
long long __isoc23_strtoll(const char *s, char **end, int base) { return strtoll(s, end, base); }
unsigned long long __isoc23_strtoull(const char *s, char **end, int base) { return strtoull(s, end, base); }

int __isoc23_sscanf(const char *s, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vsscanf(s, fmt, ap);
    va_end(ap);
    return n;
}
