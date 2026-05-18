# Porkbun nix-hapi provider module.
#
# Declares typed options under `services.nix-hapi-porkbun` and contributes
# them as a tree to `services.nix-hapi.trees.porkbun`.  No systemd /
# launchd units are declared here — the upstream `services.nix-hapi`
# module owns service activation and handles the systemd-vs-launchd split
# per platform.  Anything genuinely platform-specific belongs in the
# `nixos.nix` / `darwin.nix` wrappers, not here.
#
# Example:
#
#   services.nix-hapi.enable = true;
#   services.nix-hapi-porkbun = {
#     enable = true;
#     scopes."example.com" = {
#       provider = {
#         api_key        = mkManagedFromPath "/run/.../api-key";
#         secret_api_key = mkManagedFromPath "/run/.../api-secret";
#       };
#       ignore = [ ".key | startswith(\"NS/\")" ];
#       records = {
#         mail-mx = {
#           type    = "MX";
#           name    = "@";          # `@` is the zone apex.
#           content = "mail.example.com";
#           prio    = "10";
#         };
#         apex-spf = {
#           type    = "TXT";
#           name    = "@";
#           content = "v=spf1 mx ~all";
#         };
#         blog-cname = {
#           type    = "CNAME";
#           name    = "blog";
#           content = "user.github.io";
#         };
#       };
#     };
#   };
#
# The outer attribute name of each record (`mail-mx`, `apex-spf`, …) is a
# user-chosen label only — the reconciler matches records by the
# `__nixhapi.providerKey` derived from `type`, `name`, and `content`.
{
  self,
  nixHapiLib,
}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.nix-hapi-porkbun;
  value = nixHapiLib.types.value;

  recordType = lib.types.submodule ({config, ...}: {
    options = {
      type = lib.mkOption {
        type = lib.types.str;
        description = "DNS record type (`A`, `AAAA`, `CNAME`, `MX`, `TXT`, `NS`, …).";
      };

      name = lib.mkOption {
        type = lib.types.str;
        description = "Domain-relative record name.  Use `@` for the zone apex.";
      };

      content = lib.mkOption {
        type = value;
        description = ''
          Record content: IP for `A`/`AAAA`, hostname for `CNAME`/`MX`/`NS`,
          text payload for `TXT`, and so on.
        '';
      };

      ttl = lib.mkOption {
        type = lib.types.nullOr value;
        default = null;
        description = ''
          Record TTL, in seconds, encoded as a string.  When null, the
          field is omitted from the wire and Porkbun applies its API
          default.
        '';
      };

      prio = lib.mkOption {
        type = lib.types.nullOr value;
        default = null;
        description = ''
          Record priority, encoded as a string.  Meaningful only for
          record types that carry one (`MX`, `SRV`).
        '';
      };

      __nixhapi = lib.mkOption {
        type = lib.types.attrs;
        default = {
          providerKey = [
            (
              {
                inherit (config) type name;
              }
              // (
                if config.content ? value
                then {inherit (config.content) value;}
                else
                  throw ''
                    Porkbun record ${config.type}/${config.name}: content has
                    no statically-known value, so it cannot be included in
                    the default providerKey.  Set __nixhapi.providerKey
                    explicitly to make this record's identity unambiguous:

                      __nixhapi.providerKey = [
                        { type = "${config.type}";
                          name = "${config.name}";
                          content = "<stable-identifier>"; }
                      ];
                  ''
              )
            )
          ];
        };
        defaultText =
          lib.literalExpression
          ''{ providerKey = [ { inherit type name; content = content.value; } ]; }'';
        description = ''
          The default `providerKey` is `[ { inherit type name; content } ]`
          — the triple that distinguishes records across DNS types that
          permit multiples on the same `(type, name)`: `A`, `AAAA`, `MX`,
          `NS`, `TXT`, `SRV`.
        '';
      };
    };
  });

  providerCredsType = lib.types.submodule {
    options = {
      api_key = lib.mkOption {
        type = value;
        description = ''
          Porkbun API key.  Source via `mkManagedFromPath` or
          `mkManagedFromEnv` — inlining the key as a bare string places
          it in the world-readable Nix store.
        '';
      };
      secret_api_key = lib.mkOption {
        type = value;
        description = ''
          Porkbun secret API key.  Source via `mkManagedFromPath` or
          `mkManagedFromEnv` — inlining the key as a bare string places
          it in the world-readable Nix store.
        '';
      };
    };
  };

  scopeType = lib.types.submodule {
    options = {
      provider = lib.mkOption {
        type = providerCredsType;
        description = "Per-scope Porkbun API credentials.";
      };
      ignore = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [];
        description = ''
          jq expressions evaluated against each unowned live record's body.
          Any expression evaluating truthy exempts that record from
          deletion when it is absent from the desired state.
        '';
      };
      __nixhapi = lib.mkOption {
        type = lib.types.attrs;
        default = {};
        description = ''
          Additional engine-visible metadata for this scope's
          `__nixhapi` block.  `provider` and `ignore` are populated by
          this module from the dedicated options above and take
          precedence over same-named keys here; everything else flows
          through to the wire untouched, so any nix-hapi directive the
          engine understands now or in the future can be attached
          without a module change.
        '';
      };
      records = lib.mkOption {
        type = lib.types.attrsOf recordType;
        default = {};
        description = ''
          Per-record declarations.  The attribute key is a user-chosen
          label; record identity is derived from `type`, `name`, and
          `content` via the default providerKey.
        '';
      };
    };
  };

  # Translate one record submodule into a wire-format keyed-node body.
  # The reconciler derives `type` and `name` from `__nixhapi.providerKey`,
  # so we keep them out of the body to avoid emitting redundant fields
  # that the diff engine would otherwise consider.  Null `ttl`/`prio` are
  # stripped so the reconciler treats them as unset (no diff against the
  # corresponding live field) rather than as `null` values to enforce.
  # `record.__nixhapi` is whatever the option system merged together
  # (providerKey default + any user additions like dependsOn), so it
  # passes through verbatim — we never project our own view of which
  # directives are valid onto the namespace.
  recordToJson = record: let
    body = lib.filterAttrs (_: v: v != null) {
      inherit (record) content ttl prio;
    };
  in
    body // {__nixhapi = record.__nixhapi;};

  # Translate one scope into the wire-format tree the reconciler expects:
  # an instance-level `__nixhapi` block carrying provider config plus
  # whatever else the user has attached, then each record as a keyed-node
  # sibling.  The order `scope.__nixhapi // derived` ensures user-supplied
  # directives the module does not understand still flow through, while
  # the keys this module owns (provider, ignore) always reflect the
  # dedicated options.
  scopeToTree = domain: scope:
    {
      __nixhapi =
        scope.__nixhapi
        // {
          provider = {
            type = "porkbun";
            domain = nixHapiLib.mkManaged domain;
            inherit (scope.provider) api_key secret_api_key;
          };
          ignore = scope.ignore;
        };
    }
    // (lib.mapAttrs (_: recordToJson) scope.records);
in {
  options.services.nix-hapi-porkbun = {
    enable = lib.mkEnableOption "Porkbun DNS reconciler via nix-hapi";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression ''nix-hapi-provider-porkbun.packages.''${system}.default'';
      description = "The Porkbun reconciler binary package.";
    };

    scopes = lib.mkOption {
      type = lib.types.attrsOf scopeType;
      default = {};
      description = ''
        Per-domain scopes.  The outer attribute key is the domain
        (e.g. "example.com").  Records within a scope are accumulated
        across all modules that contribute to it.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    services.nix-hapi.trees.porkbun = {
      providers.porkbun = lib.getExe cfg.package;
      desiredState = lib.mapAttrs scopeToTree cfg.scopes;
    };
  };
}
