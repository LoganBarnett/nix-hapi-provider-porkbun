# NixOS entry point for the Porkbun nix-hapi provider module.  The
# option schema and tree-building live in `./common.nix`; this file is
# the seam where NixOS-only declarations (e.g. systemd drop-ins) would
# go if they ever became necessary.
args: import ./common.nix args
