/* The C library calls OpenThread makes, for the EM357's RCP: newlib's bring
   malloc and stdio state the EM357's RAM has no room for. snprintf and
   vsnprintf (its version and a few strings) take flags '-' and '0', a width,
   the 'l' and 'h' lengths, and %d %i %u %x %X %c %s %p %%; errno is one int. */
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>

int vsnprintf(char *out, size_t size, const char *format, va_list args)
{
  size_t n = 0;
#define PUT(c) do { if (n + 1 < size) out[n] = (c); n++; } while (0)
  for (const char *f = format; *f; f++) {
    if (*f != '%') { PUT(*f); continue; }
    int left = 0, zero = 0, width = 0;
    for (f++; *f == '-' || *f == '0'; f++) { left |= *f == '-'; zero |= *f == '0'; }
    for (; *f >= '0' && *f <= '9'; f++) width = width * 10 + (*f - '0');
    while (*f == 'l' || *f == 'h') f++;
    char digits[24];
    const char *text = digits;
    int length = 0, negative = 0;
    switch (*f) {
    case 'd': case 'i': case 'u': case 'x': case 'X': case 'p': {
      unsigned base = (*f == 'x' || *f == 'X' || *f == 'p') ? 16 : 10;
      uint32_t value;
      if (*f == 'd' || *f == 'i') {
        int32_t v = va_arg(args, int32_t);
        negative = v < 0;
        value = negative ? (uint32_t)-v : (uint32_t)v;
      } else if (*f == 'p') {
        value = (uint32_t)(uintptr_t)va_arg(args, void *);
      } else {
        value = va_arg(args, uint32_t);
      }
      const char *alphabet = *f == 'X' ? "0123456789ABCDEF" : "0123456789abcdef";
      char *end = digits + sizeof digits, *p = end;
      do { *--p = alphabet[value % base]; value /= base; } while (value);
      if (negative) *--p = '-';
      text = p;
      length = (int)(end - p);
      break;
    }
    case 'c':
      digits[0] = (char)va_arg(args, int);
      length = 1;
      break;
    case 's':
      text = va_arg(args, const char *);
      if (!text) text = "(null)";
      while (text[length]) length++;
      break;
    case '%':
      digits[0] = '%';
      length = 1;
      break;
    default:
      if (!*f) f--;
      continue;
    }
    int pad = width > length ? width - length : 0;
    if (!left) while (pad-- > 0) PUT(zero && *f != 's' && *f != 'c' ? '0' : ' ');
    for (int i = 0; i < length; i++) PUT(text[i]);
    if (left) while (pad-- > 0) PUT(' ');
  }
  if (size) out[n < size ? n : size - 1] = 0;
  return (int)n;
#undef PUT
}

int snprintf(char *out, size_t size, const char *format, ...)
{
  va_list args;
  va_start(args, format);
  int n = vsnprintf(out, size, format, args);
  va_end(args);
  return n;
}

int *__errno(void)
{
  static int error;
  return &error;
}
