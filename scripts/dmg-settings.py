# Layout of the installer disk image: Nexus on the left, a shortcut to
# Applications on the right, over assets/dmg-background.png.
# Read by dmgbuild, which scripts/bundle.sh runs; `defines` comes from it.

format = "UDZO"
files = [defines["app"]]
symlinks = {"Applications": "/Applications"}
icon = defines["icon"]
# dmgbuild also picks up the @2x image beside this one.
background = defines["background"]

window_rect = ((200, 120), (640, 400))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False

arrange_by = None
icon_size = 128
text_size = 13
# These match the arrow drawn on the background.
icon_locations = {
    "Nexus.app": (170, 190),
    "Applications": (470, 190),
}
