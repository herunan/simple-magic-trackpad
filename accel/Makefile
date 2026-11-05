CFLAGS ?= -O2 -g -Wall -Wextra -Werror
PKG_CFLAGS := $(shell pkg-config --cflags libinput)

libmac-accel-shim.so: mac-accel-shim.c
	$(CC) $(CFLAGS) $(PKG_CFLAGS) -fPIC -shared -o $@ $< -ldl

test-shim: test-shim.c
	$(CC) $(CFLAGS) $(PKG_CFLAGS) -o $@ $< $(shell pkg-config --libs libinput libudev)

clean:
	rm -f libmac-accel-shim.so test-shim

.PHONY: clean
