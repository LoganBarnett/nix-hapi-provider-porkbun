# nix-darwin entry point for the Porkbun nix-hapi provider module.  The
# option schema and tree-building live in `./common.nix`; this file is
# the seam where nix-darwin-only declarations (e.g. launchd drop-ins)
# would go if they ever became necessary.
args: import ./common.nix args
