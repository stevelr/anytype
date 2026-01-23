# Keystores

Keystores store authentication tokens for http and grpc endpoints. Various implementations store keys in memory, on disk, or in the OS Keyring

The internal interface for a keystore is defined by the `keyring_core` crate. Data is stored as a tuple (service, user, secret).

- "service", is a string name, used to keep keys isolated between different applications. Service is also displayed in OS prompts to allow access to data in keyrings.
- "user" represents the kind of secret, to distinguish, for example, an http auth token from a grpc session token.
- "secret" is the value of the token. In anytype, secrets are always valid utf8 strings.

Keystores can store keys for multiple processes and multiple service ids.

See [below](#debugging) for diagnostic and debugging tips

## Keystore spec

Keystore spec is determined by, in order of precedence, `ClientConfig::keystore`; the environment variable `ANYTYPE_KEYSTORE`; or the platform default keyring. (`keyring` for macos, `windows` for windows, `keyutils` for linux, etc)

The first word of the keystore spec is the name of the keystore: 'file', 'env', or one of the OS keyrings. See [the keyring crate](https://github.com/open-source-cooperative/keyring-rs/blob/main/src/lib.rs). for the list of platform-specific defaults and options.

If the keystore supports "modifiers" (settings), the name may be followed by a ':' (colon) and one or more key=value settings, separated by colons.

Keystore spec examples:

- `file` use file (sqlite) keystore in the default location
- `file:path=/path/to/my/file.db"` use file (sqlite) keystore in custom location
- `file:path=/path/to/my/file.db:cipher=aegis256:hexkey=HEX_KEY` file keystore in custom location with 256-bit encryption
- `secret-service` on linux, use the dbus-based secret service keystore
- `keyutils` linux kernel keyutils keystore (default on linux)
- `keyring` macos user keyring (default on macos)
- `windows` Windows Credential Store (default on windows)
- `env` retrieve keys from environment to store in an in-memory hashtable. This keystore does not persist, and accepts no modifiers.

## Service

"Service", is a string name, set in ClientConfig::keystore_service or `ANYTYPE_KEYSTORE_SERVICE`, and used to keep keys isolated between different applications. The service name is displayed in prompts from the OS to allow access to OS keyring stores.

Note that **access tokens are specific to endpoints**. If you change endpoint urls, you need to change tokens.

## File keystore

The "file" keystore stores data in a local sqlite database, with optional at-rest encryption, implemented by [db-keystore](https://docs.rs/db-keystore/latest/db_keystore/). Modifiers for the file keystore are documented in that crate's README and [docs](https://docs.rs/db-keystore/latest/db_keystore/). The most common option is "path" which sets the path to the db file. The file keystore supports encryption (encrypted data at-rest) by setting `cipher` and `hexkey`.

### Encryption

Enable encryption on the `file` keystore in one of the following ways:

- (in code) set `EncryptionOpts::cipher` and `EncryptionOpts::hexkey`
- (with anyr cli) use `--keystore file:cipher=aegis256:hexkey=HEXKEY`
- (in environment) `ANYTYPE_KEYSTORE=file:cipher=aegis256:hexkey=HEXKEY`

Supported ciphers include `aegis256` (recommended for most uses) and `aes256gcm`. See [Turso Database Encryption](https://docs.turso.tech/tursodb/encryption) for more info and options. For a 256-bit key, hexkey is 64 hex digits. A key can be generated with `openssl rand -hex 32`

## Env keystore

If the `env` keystore is used, keys are retrieved from the environment. This type of keystore may be useful for environments like github actions or hosted server environments where you can set process secrets, and don't want any persistence at all.

- `ANYTYPE_KEY_HTTP_TOKEN` - required for http authentication token
- `ANYTYPE_KEY_ACCOUNT_KEY` - account key for grpc authentication
- `ANYTYPE_KEY_SESSION_TOKEN` - session token for grpc authentication

For HTTP endpoint authentication, the http token is required. For gRPC authentication, either the account key or session token is required.

## Setting keys in the keystore

For authenticating with the desktop app, call `authenticate_interactive`, which causes the app to display a 4-digit code that the the user must enter in the console to generate an http token, which is then stored in the keystore. This is the same function used when you run `anyr auth login`. If you want to use anyr to generate an http auth token for a different application, set `ANYTYPE_KEYSTORE_SERVICE` before running `anyr auth login`.

For gRPC authentication, it is strongly recommended to use the headless cli server. To add a key to a keystore for use with the cli, use the `anyr` tool. (`anyr auth set-http`, `anyr auth set-grpc`, etc.) See [anyr](https://github.com/stevelr/anytype/tree/main/anyr) for details.

The script `../scripts/init-cli-keys.sh` can be used to initializing the cli and save gRPC and http authentication tokens to the keystore.

## Debugging Tips

- **Check:** When credentials and endpoint environment variables are correct, `anyr auth status --pretty` reports both ping checks "ok" (or just the http ping check if you don't need grpc apis)

- Try setting `ClientConfig::keystore_service = Some("anyr".to_string())` or `export ANYTYPE_KEYSTORE_SERVICE=anyr`. The default service name is taken from `ClientConfig::app_name`. Setting it to `anyr` lets the app use the same tokens as `anyr`. On the same machine where Anytype desktop runs, establish http credentials with `anyr auth login`.

- If you're using the headless cli server, `export ANYTYPE_URL=http://127.0.0.1:31012` (the default url for http endpoint is for desktop server on port 31009).

- Note that **access tokens are specific to endpoints**. If you change endpoint urls, you need to change tokens.

- If you're doing development and/or don't want to deal with pop-up approval prompts, set

```
export ANYTYPE_KEYSTORE=file
export ANYTYPE_KEYSTORE_SERVICE=anyr
```

- When using the gRPC backend, it is strongly recommended to use the headless cli server. If the file ~/.anytype/config.json contains accountKey and sessionToken, run `anyr auth set-grpc --config ~/.anytype/config.json` to set grpc credentials; or, if those fields aren't in config.json, read and use [init-cli-keys.sh](../scripts/init-cli-keys.sh) to set grpc and http credentials.
