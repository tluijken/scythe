# Privacy policy

This program will not transfer any information to other networked systems unless specifically
requested by the user or the person installing or operating it.

Scythe collects no telemetry, sends no crash reports and does not check for updates. It connects
to another system only in these cases, each started by you:

- **TONE3000.** When you sign in to TONE3000 in the NAM panel, Scythe signs in through your web
  browser (OAuth), and then searches and downloads tones from tone3000.com when you ask it to. Its
  sign-in tokens stay on your computer. Signing out removes them.
- **Control API.** scythe-server, and Scythe itself when you start it with `--control`, listen
  at the address you configure (by default only on your own computer). Devices you connect to
  it, such as a phone for in-ear mixes, talk to it over your own network.
- **scythe-mcp.** The MCP server runs only when an MCP client you set up starts it, and talks to a
  scythe-server at the address you give it.
- **Plugins and scripts.** Audio plugins and scripts you load run with your permissions and may
  use the network on their own; Scythe does not control what they do.

Your projects, recordings and settings stay on your computer.

Questions: open an issue at <https://github.com/tluijken/scythe/issues>.
