# Anytype Rust Tools and Clients

This repository is a rust workspace for Anytype automation, with client libraries and cli tools.

## Projects

<table>
<tr>
<td width="50%" valign="top">
<h3><a href="./anytype-api/">📦 anytype-api</a></h3>
<p>An ergonomic Anytype API client in Rust</p>
</td>
<td width="50%" valign="top">
<h3><a href="./anyr/">⌨️ anyr</a></h3>
<p>List, search, and manipulate Anytype objects from the command line</p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<h3><a href="./any-edit/">✏️ any-edit</a></h3>
<p>Edit Anytype documents in an external editor</p>
</td>
<td width="50%" valign="top">
<h3><a href="./anytype-rpc/">🔌 anytype-rpc</a></h3>
<p>Experimental Rust gRPC client for Anytype</p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<h3><a href="./any-mcp/">🔗 any-mcp</a></h3>
<p>Bounded, workflow-oriented MCP server for Anytype</p>
</td>
<td width="50%" valign="top">
<h3><a href="./anyback/">💾 anyback</a></h3>
<p>Backup, restore, and inspect Anytype spaces</p>
</td>
</tr>
</table>

## Compatibility notes

- [Numeric and checkbox filter status](FILTER_STATUS.md) records the
  supported condition, value-encoding, and endpoint matrix plus the disposition
  of the historical upstream parsing bug.
