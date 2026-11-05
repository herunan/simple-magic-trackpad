/* Offline check for the shim: open a touchpad with libinput's path
 * backend the way KWin would (device added, then the saved profile set),
 * and report what libinput ends up with. Run with the shim preloaded:
 *   LD_PRELOAD=./libmac-accel-shim.so ./test-shim /dev/input/eventN
 * Reads nothing from the device; safe to run against the live clone. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <libinput.h>
#include <stdio.h>
#include <unistd.h>

static int open_restricted(const char *path, int flags, void *data)
{
	(void)data;
	return open(path, flags);
}
static void close_restricted(int fd, void *data)
{
	(void)data;
	close(fd);
}
static const struct libinput_interface iface = { open_restricted, close_restricted };

static const char *name(enum libinput_config_accel_profile p)
{
	switch (p) {
	case LIBINPUT_CONFIG_ACCEL_PROFILE_FLAT: return "flat";
	case LIBINPUT_CONFIG_ACCEL_PROFILE_ADAPTIVE: return "adaptive";
	case LIBINPUT_CONFIG_ACCEL_PROFILE_CUSTOM: return "custom";
	default: return "none";
	}
}

int main(int argc, char **argv)
{
	if (argc != 2) {
		fprintf(stderr, "usage: %s /dev/input/eventN\n", argv[0]);
		return 2;
	}
	struct libinput *li = libinput_path_create_context(&iface, NULL);
	struct libinput_device *dev = libinput_path_add_device(li, argv[1]);
	if (!dev) {
		fprintf(stderr, "cannot open %s\n", argv[1]);
		return 1;
	}
	libinput_dispatch(li);
	struct libinput_event *ev;
	while ((ev = libinput_get_event(li)))
		libinput_event_destroy(ev);

	/* the unwrapped getter, to see libinput's real state */
	__typeof__(libinput_device_config_accel_get_profile) *real_get =
		dlsym(RTLD_NEXT, "libinput_device_config_accel_get_profile");
	if (!real_get || real_get == libinput_device_config_accel_get_profile)
		real_get = dlsym(RTLD_DEFAULT, "libinput_device_config_accel_get_profile");
	void *h = dlopen("libinput.so.10", RTLD_NOW | RTLD_NOLOAD);
	if (h)
		real_get = dlsym(h, "libinput_device_config_accel_get_profile");

	printf("%s after add: reported %s, real %s\n", libinput_device_get_name(dev),
	       name(libinput_device_config_accel_get_profile(dev)), name(real_get(dev)));
	enum libinput_config_status st =
		libinput_device_config_accel_set_profile(dev, LIBINPUT_CONFIG_ACCEL_PROFILE_ADAPTIVE);
	printf("set adaptive -> %s: reported %s, real %s\n", libinput_config_status_to_str(st),
	       name(libinput_device_config_accel_get_profile(dev)), name(real_get(dev)));
	st = libinput_device_config_accel_set_profile(dev, LIBINPUT_CONFIG_ACCEL_PROFILE_FLAT);
	printf("set flat     -> %s: reported %s, real %s\n", libinput_config_status_to_str(st),
	       name(libinput_device_config_accel_get_profile(dev)), name(real_get(dev)));
	libinput_unref(li);
	return 0;
}
