# anytype-cli

Command-line interface for the Anytype local API, built on [`anytype`](https://github.com/stevelr/anytype).

## Install

```sh
cargo install anyr
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

- **Desktop**: If the Anytype desktop app is running, type `anyr auth login` and the app will display a 4-digit code. Enter the code into the cli prompt, and a key is generated and stored in the KeyStore.

- **Headless server**: If you are using the headless cli server, start the server, run `anytype auth apikey create anyr` to generate and display a key, save it in a file, and set the key file path as described in [Key storage](#key-storage).

## Run

```sh
# show options
anyr --help

# check authentication status
anyr auth status

# List spaces your user is authorized to access
anyr space list -t

# List spaces with json output
anyr space list            # default output is json
anyr space list --json     # same as default
anyr space list --pretty   # json formatted
```

## Common options

- `--help` show context-specific help

**Configuration**

- `--url` anytype endpoint url
- `--keyfile`, `--keyfile-path`, `--keyring`, `--keyring-service`: see [Key storage](#key-storage)

**Output format**

- `--json` (same as default).
- `--pretty` (json pretty-print)
- `-t/--table` (table format, easy to read in terminal)
- `--quiet` (minimal output)

**Filters**

- `--filter` - add filters
- `--type` - limit search results to type(s)
- `--sort` - sort results by property key (ascending)
- `--desc` - if used with `--sort`, sort descending

- There are some apparent bugs in anytype-heart that limit the functionality of search filters, especially in 'list' commands. See [Troubleshooting](../anytype-api/Troubleshooting.md) for current known issues.

## Examples

### List objects in a space

```sh
# List <ENTITY> in a space. (entities: object, member, property, template)
# anyr <ENTITY> list <SPACE_ID_OR_NAME>

# list objects in space 'Personal'
anyr object list "Personal" -t

# list types in space 'Personal'
anyr type list "Personal" -t

```

### Search in space

```sh
# search space "Work" for tasks containing the text "customer"
anyr search --space "Work" --type Task --text customer -t
```

### List tasks in space

```sh
space_id="Work" # specify space using name or id
for task in `anyr search --type Task --space $space_id --json | jq -r '.items[] | .id`; do
  data=$(anyr object get $space_id $task --json)
  status=$(jq -r '.properties[] | select (.key=="status") .select.name' <<< "$data")
  name=$(jq -r '.name' <<< "$data")
  # get created_date as YYYY-MM-DD
  created_date=$(jq -r '.properties[] | select (.key=="created_date") .date' <<< "$data" | sed 's/T.*$//')
  # generate formatted table with date, status, and name
  printf "%10s %-12s %s" $created_date $status $name
done
```

### Get objects from a collection list or grid view.

If you have a list or grid formatted view, you can use `view objects` to list the view items by specifying the space name, list, and view.

- Results are filtered and sorted by the criteria in the view.
- View can be specified by the view id or view name.
- The --json and --pretty format outputs include all properties of the objects.

Table listing features for `view objects`:

- Table listing defaults to name column only. Specify columns in table output with `--cols/--columns` and a comma-separated list of property keys. Example `--cols name,creator,created_date,status`
- Format dates with strftime format: `--date-format` or `ANYTYPE_DATE_FORMAT`, defaults to `%Y-%m-%d %H:%M:%S`.
- Members names are displayed instead of member id.

Example: get all tasks in space "Work" from view "All"

```sh
# show names of all tasks in space "Work", using view 'All'
anyr view objects --view All "Work" Task -t

# show columns: Name, Created By, and Status (note: column names are specified by property_key)
anyr view objects --view All "Work" Task --cols name,creator,status

# get tasks from view ByProject in json, with all properties
anyr view objects --view ByProject "Work" Task --json
```

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
RUST_LOG=debug anyr object list <SPACE_ID>
```

Log HTTP requests and responses:

```sh
RUST_LOG=warn,anytype::http_json=trace anyr object list <SPACE_ID>
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

Licensed under either of:

- Apache License, Version 2.0 (`LICENSE-APACHE`)
- MIT License (`LICENSE-MIT`)
