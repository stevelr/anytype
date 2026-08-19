+++
title = "Keystores"
weight = 20
+++

# Store Anytype credentials

Anytype Toolbox stores HTTP tokens and gRPC account or session credentials in
a selected keystore. `ANYTYPE_KEYSTORE` or `anyr --keystore SPEC` selects the
backend. `ANYTYPE_KEYSTORE_SERVICE` or `--keystore-service NAME` selects the
credential namespace and defaults to `anyr`.

Endpoint tokens are not interchangeable. When you change the HTTP endpoint,
authenticate for that endpoint and store its token under the intended service.

## Keystore specifications

A specification starts with a backend name. Backends that accept settings use
colon-separated `key=value` modifiers.

| Specification | Storage |
| --- | --- |
| `file` | SQLite database in the default application location |
| `file:path=/private/keys.db` | SQLite database at an explicit path |
| `secret-service` | Linux Secret Service |
| `keychain` | macOS Keychain |
| `windows` | Windows Credential Manager |
| `env` | Process environment, with no persistence |

The platform default uses the operating-system credential store when one is
available. `keyutils` is also supported on Linux, but its keys do not survive a
reboot.

## File keystore encryption

The file backend supports encrypted SQLite storage:

```sh
export ANYTYPE_KEYSTORE='file:path=/private/keys.db:cipher=aegis256:hexkey=HEX_KEY'
```

`aegis256` is the usual cipher choice; `aes256gcm` is also supported. A 256-bit
key is 64 hexadecimal digits. Generate one with:

```sh
openssl rand -hex 32
```

Keep the encryption key outside the database and outside committed shell or
service configuration. See the
[db-keystore documentation](https://docs.rs/db-keystore/latest/db_keystore/)
for supported file-backend modifiers.

## Environment keystore

`ANYTYPE_KEYSTORE=env` reads credentials into memory without writing them to
disk:

| Variable | Credential |
| --- | --- |
| `ANYTYPE_KEY_HTTP_TOKEN` | HTTP access token |
| `ANYTYPE_KEY_ACCOUNT_KEY` | gRPC account key |
| `ANYTYPE_KEY_SESSION_TOKEN` | gRPC session token |

HTTP requires `ANYTYPE_KEY_HTTP_TOKEN`. gRPC requires either the account key or
the session token. Supply these values through a process secret facility; do
not put them in prompts, command arguments, logs, or committed files.

`anyr init-cli --save-env FILE` can create an owner-only POSIX shell file for a
headless environment. The file contains plaintext credentials and refuses to
replace an existing path.

## Diagnose credential selection

```sh
anyr auth status --pretty
```

If another application should reuse `anyr` credentials, select the same
keystore and set its service to `anyr`. If the credential is present but the
ping fails, verify the endpoints in the [connection reference](/reference/connections/).
