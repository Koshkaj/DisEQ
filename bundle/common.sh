#!/bin/bash
# Shared helpers for the bundle and disk-image scripts. Sourced, not run.
#
# The workspace `Cargo.toml` is the single source of truth for the version. The
# Info.plists are stamped from it at build time, so a released bundle can never
# disagree with the crate it was built from — or with the tag it was cut at.

# Prints the version from [workspace.package].
diseq_version() {
	local root="$1"
	sed -n '/^\[workspace\.package\]/,/^\[/p' "$root/Cargo.toml" \
		| sed -n 's/^version *= *"\(.*\)"/\1/p' \
		| head -1
}

# Writes that version into an Info.plist that has already been copied into a
# bundle. Never call this on a tracked source plist.
stamp_version() {
	local plist="$1" version="$2"
	/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $version" "$plist"
	/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$plist"
}
