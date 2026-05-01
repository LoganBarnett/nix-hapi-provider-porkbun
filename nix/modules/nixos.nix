# NixOS module for the Porkbun nix-hapi provider.
#
# Declares typed options under `services.nix-hapi-porkbun` and contributes
# a `services.nix-hapi.trees.porkbun` tree to the engine.  Users write
# declarative config; the module translates it into the JSON the rust
# reconciler expects on stdin.
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
#         "MX/@"        = { content = "mail.example.com"; prio = "10"; };
#         "TXT/@"       = { content = "v=spf1 mx ~all"; };
#         "CNAME/blog"  = { content = "user.github.io"; };
#       };
#     };
#   };
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

  recordType = lib.types.submodule {
    options = {
      content = lib.mkOption {
        type = value;
        description = "Record content.  Bare strings are treated as managed.";
      };
      ttl = lib.mkOption {
        type = lib.types.nullOr value;
        default = null;
      };
      prio = lib.mkOption {
        type = lib.types.nullOr value;
        default = null;
      };
    };
  };

  providerCredsType = lib.types.submodule {
    options = {
      api_key = lib.mkOption {
        type = value;
        description = "Porkbun API key, typically wrapped via mkManagedFromPath.";
      };
      secret_api_key = lib.mkOption {
        type = value;
        description = "Porkbun secret API key, typically wrapped via mkManagedFromPath.";
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
          jq expressions matching record keys (TYPE/name) the reconciler
          should leave unchanged on every apply.
        '';
      };
      records = lib.mkOption {
        type = lib.types.attrsOf recordType;
        default = {};
        description = ''
          Map of "TYPE/relative_name" to record fields.  Multiple modules
          may contribute records to the same scope; they merge per-key
          and collisions on the same record key error at evaluation time.
        '';
      };
    };
  };

  # Translate one typed scope into the JSON shape the rust reconciler
  # expects today: provider config and ignore list tunneled under
  # `__nixhapi`, records spread at the top level of the scope.  The
  # outer attribute key supplies the domain.
  #
  # Wire-format details we preserve to avoid rust-side changes:
  #   * `domain` is wrapped as a managed value (matching the historical
  #     output of mkPorkbunProvider), not emitted as a bare string.
  #   * Null ttl / prio are stripped per record, matching the historical
  #     conditional emission rather than emitting explicit nulls.
  recordToJson = record:
    lib.filterAttrs (_: v: v != null) {
      inherit (record) content ttl prio;
    };

  scopeToTree = domain: scope:
    {
      __nixhapi = {
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
