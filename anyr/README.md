# anyr

List, search, and manipulate anytype objects from the command-line

Homepage: https://github.com/stevelr/anytype

```sh
# show options
anyr --help

# check authentication status
anyr auth status

# List spaces your user is authorized to access
anyr space list -t         # output as table (-t/--table)
```

## Common options

<table>
    <tbody>
      <tr>
        <td></td>
        <td><code>--help</code></td>
        <td>show context-specific help</td>
      </tr>
      <tr>
        <td>Endpoint URL</td>
        <td><code>--url URL</code></td>
        <td>Anytype endpoint url</td>
      </tr>
      <tr>
        <td rowspan="3">Key Storage</td>
        <td><code>--keyfile</code></td>
        <td>key stored in file in standard location</td>
      </tr>
      <tr>
        <td><code>--keyfile-path PATH</code></td>
        <td>key stored at PATH</td>
      </tr>
      <tr>
        <td><code>--keyring</code></td>
        <td>key stored in OS keyring</td>
      </tr>
      <tr>
        <td rowspan="3">Output Format</td>
        <td>[ <code>--json</code> ]</td>
        <td>json formatted output (the default)</td>
      </tr>
      <tr>
        <td><code>-t</code>, <code>--table</code></td>
        <td>table format</td>
      </tr>
      <tr>
        <td><code>--pretty</code></td>
        <td>json indented</td>
      </tr>
      <tr>
        <td rowspan="4">Filters</td>
        <td><code>--filter KEY=VALUE</code></td>
        <td>add filter condition(s)</td>
      </tr>
      <tr>
        <td><code>--type TYPE</code></td>
        <td>add type constraint(s)</td>
      </tr>
      <tr>
        <td><code>--sort KEY</code></td>
        <td>sort on key</td>
      </tr>
      <tr>
        <td><code>--desc</code></td>
        <td>sort descending</td>
      </tr>
    </tbody>
  </table>

## Examples

**List objects in a space**

```sh
# List <ENTITY> in a space. (entities: object, member, property, template)
# anyr <ENTITY> list <SPACE_ID_OR_NAME>

# list objects in space 'Personal'
anyr object list "Personal" -t

# list types in space 'Personal'
anyr type list "Personal" -t

```

**Search in space**

```sh
# search space "Work" for tasks containing the text "customer"
anyr search --space "Work" --type Task --text customer -t
```

**List tasks in space**

```sh
space="Work" # specify space using name or id
for task in `anyr search --type Task --space $space --json | jq -r '.items[] | .id`; do
  data=$(anyr object get $space $task --json)
  status=$(jq -r '.properties[] | select (.key=="status") .select.name' <<< "$data")
  name=$(jq -r '.name' <<< "$data")
  # get created_date as YYYY-MM-DD
  created_date=$(jq -r '.properties[] | select (.key=="created_date") .date' <<< "$data" | sed 's/T.*$//')
  # generate formatted table with date, status, and name
  printf "%10s %-12s %s" $created_date $status $name
done
```

**List items in query or collection**

```sh
# list queries in space. "$space" can be id ("bafy...") or name ("Projects")
anyr search --type set --space $space -t
# list collections in the space
anyr search --type collection --space $space -t
# from above, get id of query or collection of interest, then
# list items in query or collection, in view "All"
anyr view objects --view All $space $query_or_collection_id -t
```

**Get objects from a collection list or grid view**

```sh
# show names of all tasks in space "Work", using view 'All'
anyr view objects --view All "Work" Task -t

# show columns: Name, Created By, and Status (note: column names are specified by property_key)
anyr view objects --view All "Work" Task --cols name,creator,status

# get tasks from view ByProject in json, with all properties
anyr view objects --view ByProject "Work" Task --json
```

If you have a list or grid formatted view, you can use `view objects` to list the view items by specifying the space name, list, and view.

- Results are filtered and sorted by the criteria in the view.
- View can be specified by the view id or view name.
- The --json and --pretty format outputs include all properties of the objects.

Table listing features for `view objects`:

- Table listing defaults to name column only. Specify columns in table output with `--cols/--columns` and a comma-separated list of property keys. Example `--cols name,creator,created_date,status`
- Format dates with strftime format: `--date-format` or `ANYTYPE_DATE_FORMAT`, defaults to `%Y-%m-%d %H:%M:%S`.
- Members names are displayed instead of member id.

## Install

Release binaries are on [github](https://github.com/stevelr/anytype/tags)

**Macos via Homebrew**

```sh
brew install stevelr/tap/anyr
```

**Linux (arm64/x86_64)**

```sh
curl -fsSL https://github.com/stevelr/anytype/releases/latest/download/anyr-installer.sh | sh
```

**Windows Powershell**

```sh
irm https://github.com/stevelr/anytype/releases/latest/download/anyr-installer.ps1 | iex
```

**Cargo**

```sh
cargo install -p anyr
```

## Build from source

**Cargo**
Ensure you have 'protoc' from the protobuf package in your path. On macos, 'brew install protobuf'

```sh
cargo install -p anyr
```

**Nix**

```sh
nix build
```

## Configure

Configuration can be set with command-line parameters or environment variables.

- **Url** The default url is the desktop client `http://127.0.0.1:31009`. Override with `--url` or the environment variable `ANYTYPE_URL`.

- **Key Storage** The default key storage method should work on most platforms. Options for overriding the defaults are described below in [Key storage](#key-storage).

```sh
# use headless server and custom key path
anyr --url "http://127.0.0.1:31012" --keyfile-path "$HOME/.config/anytype/api.key" ARGS ...`

# custom url and key path in environment
export ANYTYPE_URL=http://127.0.0.1:31012
export ANYTYPE_KEY_FILE="$HOME/.config/anytype/api.key"
anyr ARGS ...
```

### Generate api key (first-time authentication)

- **Desktop**: If the Anytype desktop app is running, type `anyr auth login` and the app will display a 4-digit code. Enter the code into the anyr prompt, and a key is generated and stored in the KeyStore.

- **Headless server**: If you are using the headless cli server, start the server, run `anytype auth apikey create anyr` to generate and display a key, save it in a file, and set the key file path as described in [Key storage](#key-storage).

## Key storage

`anyr` requires an api key to access Anytype documents, which is obtained with a one-time authentication step.

First, decide which of two methods will be used for storing the key. The key should be kept secret like other passwords, as it can allow access to unencrypted anytype documents. Preferably, the api key is stored in the secure OS Keyring, which requires biometric or password authentication. Alternately, the key can be saved in a file on your drive, which is less secure.

Key storage is determined in the following order of precedence:

1. If flag `--keyfile` is present, or environment variable `ANYTYPE_KEYSTORE_FILE` is `1`, api keys are stored in a file in a config folder.

| Platform | Value                                 | Example                                               |
| -------- | ------------------------------------- | ----------------------------------------------------- |
| Linux    | `$XDG_CONFIG_HOME` or `$HOME`/.config | /home/alice/.config/anyr/api.key                      |
| macOS    | `$HOME`/Library/Application Support   | /Users/Alice/Library/Application Support/anyr/api.key |
| Windows  | `{FOLDERID_RoamingAppData}`           | C:\Users\Alice\AppData\Roaming\anyr\api.key           |
| (other)  |                                       | ./anyr/api.key                                        |

2. If arg `--keyfile-path` is set, or environment variable `ANYTYPE_KEY_FILE` is set, api keys are stored in a file with that path.

3. If flag `--keyring` is present, or `ANYTYPE_KEYSTORE_KEYRING` is `1`, the OS keyring is used and prompts user with service name "anyr".

4. If none of the above overrides are present, key storage defaults to the OS keyring for MacOS and Windows, and the default file path for Linux and other platforms.

## Logging

Debug logging

```sh
RUST_LOG=debug anyr space list -t
```

Log HTTP requests and responses:

```sh
RUST_LOG=warn,anytype::http_json=trace anyr space list -t
```

## Testing

Python CLI tests expect the same environment variables as the API tests:

- `ANYTYPE_TEST_URL` (or `ANYTYPE_URL`)
- `ANYTYPE_TEST_KEY_FILE` (or `ANYTYPE_KEY_FILE`)
- `ANYTYPE_TEST_SPACE_ID`

```sh
source .test-env
python tests/cli_commands.py
```

## License

Apache License, Version 2.0
