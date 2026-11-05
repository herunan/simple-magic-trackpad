/*
 * mac-accel-shim: give the Apple Magic Trackpad a custom libinput
 * acceleration curve inside KWin.
 *
 * KWin 6.8 only offers libinput's flat and adaptive profiles; libinput
 * itself also has a "custom" profile that takes a sampled speed curve,
 * but only the compositor can set it. This library is preloaded into
 * kwin_wayland (via a drop-in on plasma-kwin_wayland.service) and wraps
 * the few libinput calls where KWin configures a device. Whenever KWin
 * adds the trackpad or sets its acceleration profile, the custom curve
 * from ~/.config/mac-trackpad/accel.conf is applied on top.
 *
 * Only Apple Magic Trackpads are touched (vendor 0x004c or 0x05ac,
 * product 0x0265). With no readable config file, nothing changes.
 *
 * Reloading the curve without a re-login: make KWin re-set the profile,
 * e.g. toggle "Pointer acceleration" in System Settings > Touchpad, or
 * `mac-trackpad-accel reload` (sets KWin's adaptive profile over D-Bus).
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <libinput.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_POINTS 64

struct curve {
	double step;
	double points[MAX_POINTS];
	size_t npoints;
};

static void logmsg(const char *fmt, const char *arg)
{
	fprintf(stderr, "mac-accel-shim: ");
	fprintf(stderr, fmt, arg);
	fputc('\n', stderr);
}

/* Don't let anything kwin_wayland spawns (Xwayland, helpers) inherit the
 * preload. kwin_wayland_wrapper must keep it so a restarted kwin_wayland
 * gets it again, hence the name check. */
__attribute__((constructor)) static void init(void)
{
	if (strcmp(program_invocation_short_name, "kwin_wayland") == 0)
		unsetenv("LD_PRELOAD");
}

static int is_magic_trackpad(struct libinput_device *dev)
{
	if (!dev)
		return 0;
	unsigned vid = libinput_device_get_id_vendor(dev);
	unsigned pid = libinput_device_get_id_product(dev);
	return (vid == 0x004c || vid == 0x05ac) && pid == 0x0265 &&
	       libinput_device_has_capability(dev, LIBINPUT_DEVICE_CAP_GESTURE);
}

static int parse_points(char *val, struct curve *c)
{
	char *save = NULL;
	c->npoints = 0;
	for (char *tok = strtok_r(val, " \t\n", &save); tok;
	     tok = strtok_r(NULL, " \t\n", &save)) {
		if (c->npoints == MAX_POINTS)
			return -1;
		char *end;
		errno = 0;
		double v = strtod(tok, &end);
		if (errno || end == tok || v < 0)
			return -1;
		c->points[c->npoints++] = v;
	}
	return c->npoints >= 2 ? 0 : -1;
}

/* Returns 0 when both curves were read. */
static int load_config(struct curve *motion, struct curve *scroll)
{
	char path[4096];
	const char *xdg = getenv("XDG_CONFIG_HOME");
	const char *home = getenv("HOME");
	if (xdg && *xdg)
		snprintf(path, sizeof path, "%s/mac-trackpad/accel.conf", xdg);
	else if (home)
		snprintf(path, sizeof path, "%s/.config/mac-trackpad/accel.conf", home);
	else
		return -1;

	FILE *f = fopen(path, "re");
	if (!f)
		return -1;

	int have = 0;
	char *line = NULL;
	size_t cap = 0;
	while (getline(&line, &cap, f) > 0) {
		if (line[0] == '#')
			continue;
		char *eq = strchr(line, '=');
		if (!eq)
			continue;
		*eq = '\0';
		char *key = line, *val = eq + 1;
		if (strcmp(key, "motion_step") == 0) {
			motion->step = strtod(val, NULL);
			have |= 1;
		} else if (strcmp(key, "motion_points") == 0) {
			if (parse_points(val, motion) == 0)
				have |= 2;
		} else if (strcmp(key, "scroll_step") == 0) {
			scroll->step = strtod(val, NULL);
			have |= 4;
		} else if (strcmp(key, "scroll_points") == 0) {
			if (parse_points(val, scroll) == 0)
				have |= 8;
		}
	}
	free(line);
	fclose(f);

	if (have != 15 || motion->step <= 0 || scroll->step <= 0) {
		logmsg("%s is incomplete or invalid; leaving acceleration alone", path);
		return -1;
	}
	return 0;
}

/* libinput_device_config_accel_apply() calls the public
 * libinput_device_config_accel_set_profile() itself, which lands back in
 * our wrapper below; this guard stops that recursion. */
static int applying;

static void apply_custom(struct libinput_device *dev)
{
	struct curve motion, scroll;
	if (applying || load_config(&motion, &scroll) != 0)
		return;

	struct libinput_config_accel *cfg =
		libinput_config_accel_create(LIBINPUT_CONFIG_ACCEL_PROFILE_CUSTOM);
	if (!cfg)
		return;

	/* fallback covers the "unaccelerated" paths (e.g. gesture deltas);
	 * libinput's default fallback is 1:1 in device units, far too fast */
	enum libinput_config_status st = libinput_config_accel_set_points(
		cfg, LIBINPUT_ACCEL_TYPE_FALLBACK, motion.step, motion.npoints, motion.points);
	if (st == LIBINPUT_CONFIG_STATUS_SUCCESS)
		st = libinput_config_accel_set_points(cfg, LIBINPUT_ACCEL_TYPE_MOTION,
						      motion.step, motion.npoints, motion.points);
	if (st == LIBINPUT_CONFIG_STATUS_SUCCESS)
		st = libinput_config_accel_set_points(cfg, LIBINPUT_ACCEL_TYPE_SCROLL,
						      scroll.step, scroll.npoints, scroll.points);
	if (st == LIBINPUT_CONFIG_STATUS_SUCCESS) {
		applying = 1;
		st = libinput_device_config_accel_apply(dev, cfg);
		applying = 0;
	}
	libinput_config_accel_destroy(cfg);

	if (st == LIBINPUT_CONFIG_STATUS_SUCCESS)
		logmsg("applied Mac curve to %s", libinput_device_get_sysname(dev));
	else
		logmsg("libinput rejected the curve for %s", libinput_device_get_sysname(dev));
}

/* --- wrapped libinput entry points --------------------------------------- */

#define REAL(name) \
	static __typeof__(name) *real_##name; \
	if (!real_##name) \
		real_##name = (__typeof__(name) *)dlsym(RTLD_NEXT, #name)

struct libinput_event *libinput_get_event(struct libinput *li)
{
	REAL(libinput_get_event);
	struct libinput_event *ev = real_libinput_get_event(li);
	if (ev && libinput_event_get_type(ev) == LIBINPUT_EVENT_DEVICE_ADDED) {
		struct libinput_device *dev = libinput_event_get_device(ev);
		if (is_magic_trackpad(dev))
			apply_custom(dev);
	}
	return ev;
}

/* KWin applies the saved profile right after the device appears, and
 * again whenever the setting changes; re-apply the curve each time. */
enum libinput_config_status
libinput_device_config_accel_set_profile(struct libinput_device *dev,
					 enum libinput_config_accel_profile profile)
{
	REAL(libinput_device_config_accel_set_profile);
	enum libinput_config_status st =
		real_libinput_device_config_accel_set_profile(dev, profile);
	if (st == LIBINPUT_CONFIG_STATUS_SUCCESS && is_magic_trackpad(dev) &&
	    profile == LIBINPUT_CONFIG_ACCEL_PROFILE_ADAPTIVE)
		apply_custom(dev);
	return st;
}

/* Report "adaptive" to KWin while our curve is active, so its settings
 * page and D-Bus properties stay within the two profiles it knows. */
enum libinput_config_accel_profile
libinput_device_config_accel_get_profile(struct libinput_device *dev)
{
	REAL(libinput_device_config_accel_get_profile);
	enum libinput_config_accel_profile p =
		real_libinput_device_config_accel_get_profile(dev);
	if (p == LIBINPUT_CONFIG_ACCEL_PROFILE_CUSTOM && is_magic_trackpad(dev))
		return LIBINPUT_CONFIG_ACCEL_PROFILE_ADAPTIVE;
	return p;
}
