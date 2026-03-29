# Porkbun-specific helpers for constructing valid nix-hapi desired state.
# These enforce required fields at Nix eval time.  Plain string values are
# auto-wrapped as mkManaged; values already tagged with __nixhapi pass through.
let
  ensureManaged = v:
    if builtins.isAttrs v && v ? __nixhapi
    then v
    else {
      __nixhapi = "managed";
      value = v;
    };
in {
  # Constructs a Porkbun DNS provider scope for one domain.
  # Config fields accept plain strings (auto-wrapped as managed) or
  # pre-tagged values (mkManagedFromPath, mkInitial, etc.).
  # records: attrset of "TYPE/relative_name" -> mkRecord { ... }
  # ignore: list of regex strings matching record keys to leave unchanged.
  mkPorkbunProvider = {
    domain,
    api_key,
    secret_api_key,
    records ? {},
    ignore ? [],
  }:
    {
      __nixhapi =
        {
          provider = {
            type = "porkbun";
            domain = ensureManaged domain;
            api_key = ensureManaged api_key;
            secret_api_key = ensureManaged secret_api_key;
          };
        }
        // (
          if ignore != []
          then {inherit ignore;}
          else {}
        );
    }
    // records;

  # Constructs a single DNS record entry.  content is required; ttl and prio
  # are optional (the provider defaults ttl to 600 when omitted).
  mkRecord = {
    content,
    ttl ? null,
    prio ? null,
  }:
    {content = ensureManaged content;}
    // (
      if ttl != null
      then {ttl = ensureManaged ttl;}
      else {}
    )
    // (
      if prio != null
      then {prio = ensureManaged prio;}
      else {}
    );
}
