# Sourced by the devbox scripts: devbox run overrides PKG_CONFIG_PATH set in init_hook.
export PKG_CONFIG_PATH="$DEVBOX_PACKAGES_DIR/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
scythe_libdirs=$(pkg-config --variable=libdir xkbcommon wayland-client x11 xcursor xrandr xi gl vulkan jack alsa 2>/dev/null | tr ' ' ':')
export LD_LIBRARY_PATH="$scythe_libdirs${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
unset scythe_libdirs
