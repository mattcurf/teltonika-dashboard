# Teltonika Network Panel

A web dashboard for a Teltonika RUTM50 router and TSW202 switches.
It uses only the official [Teltonika Web API](https://developers.teltonika-networks.com/).

The panel shows:

- A logical network diagram with links, ports, VLANs and IP addresses.
- Router status: system, cellular, Wi-Fi, Tailscale and Ethernet ports.
- Switch status: system, ports, PoE, traffic and VLANs.
- Device events from Event Juggler.

You can also use the panel to:

- Turn cellular data, Wi-Fi and Tailscale on or off.
- Turn single ports on or off. Click a port on the faceplate to do this.

The panel does not let you turn off a port that carries its own connection to a device.
These ports show a lock symbol.

## Requirements

- Rust 1.91 or later, for the panel.
- Node.js 23.6 or later, for the device mocks and the end-to-end tests only.
- Docker with Compose, for the mock devices only.

The panel is one Rust program, `teltonika-panel`.
The web page is built into the program.

## Code layout

The project is a Cargo workspace with two crates:

- `crates/teltonika` is a library for the Teltonika Web API.
  It contains the API client, the router and switch status, the on/off controls, the Event Juggler rules and the link detection.
  It has no web server code.
- `crates/teltonika-panel` is the command-line program.
  It runs the web server with Tokio and axum, polls the devices and serves the page.

## Use physical devices

1. Create a file named `.env` in the project root:

   ```sh
   RUTM50_HOST=192.168.30.1
   RUTM50_USERNAME=admin
   RUTM50_PASSWORD=your-router-password
   TSW202_HOST=192.168.30.2
   TSW202_USERNAME=admin
   TSW202_PASSWORD=your-switch-password
   # Address of this computer, as the devices see it.
   PANEL_HOOK_URL=http://192.168.30.180:8080/api/hook
   ```

   Git ignores `.env`. Do not commit it.
   Each setting can also be an environment variable or a command-line option.
   Run `cargo run -p teltonika-panel -- --help` to see all options.

2. Start the panel:

   ```sh
   cargo run --release -p teltonika-panel
   ```

   The panel listens on port 8080.
   Set `PORT` or use `serve --port` to change it.

3. Open http://localhost:8080.

4. Create the Event Juggler rules on the devices:

   ```sh
   cargo run --release -p teltonika-panel -- setup-hooks
   ```

   This step is optional.
   Without the rules, the panel still polls the devices every 3 seconds.
   With the rules, the devices tell the panel at once when a port or mobile data changes.
   The command replaces rules named `panel_*` and keeps all other rules.
   It skips devices that do not answer, so run it again after you install a new device.

### Second switch

The panel has a card for a second TSW202 at `192.168.30.3`.
The card shows "not detected" until that switch is on the network.
Set `TSW202_2_HOST` to use another address.
The second switch uses the first switch's user name and password.
Set `TSW202_2_USERNAME` and `TSW202_2_PASSWORD` to use other values.

### Who can change device settings

On physical devices, only this computer can change settings through `/api/toggle`.
Other hosts on the LAN get HTTP 403.
Only the configured devices can send events to `/api/hook`.

## Use mock devices

The mock devices let you run and test the panel without hardware.
Each mock is one container:

| Container | Device | HTTPS port on your computer |
| --- | --- | --- |
| `rutm50` | RUTM50 router | 8441 |
| `tsw202` | TSW202 switch | 8442 |

The mocks serve the same API endpoints as the real devices, with the same response shapes.
Data comes from a RUTM50 on firmware RUTM_R_00.07.25.3 and a TSW202 on TSW2_R_00.01.11.
Serial numbers, MAC addresses and network names are fake.

The mocks behave like real devices:

- Login with a user name and password, bearer tokens and token expiry.
- Bulk requests, with HTTP 207 replies.
- Self-signed HTTPS certificates.
- Settings that you change stay changed until the container stops.
- Signal, load and traffic values change over time.
- Event Juggler rules post events to the panel when a port or mobile data changes.
- A stopped Tailscale reports empty values as `""` instead of `[]`, as the real router does.

The default mock network:

- RUTM50 LAN1 connects to TSW202 port 1 at 1 Gbps.
  This link carries VLAN 1 untagged and VLAN 10 tagged.
- TSW202 port 3 links at 100 Mbps.
- TSW202 port 5 powers a 4.2 W PoE device.
- TSW202 port 6 is an untagged VLAN 10 port.
- The second switch is not present, as on the physical network.

### Start the mock stack

```sh
npm run mock:up
```

This builds and starts three containers: the two mocks and the Rust panel.
The first build compiles the panel and takes a few minutes.
Open http://localhost:8080.
Set `PANEL_PORT` to use another port, for example `PANEL_PORT=9090 npm run mock:up`.

Stop the stack:

```sh
npm run mock:down
```

The password of both mocks is `mock-password`.
The user name is `admin`.

In the mock stack, any host can change settings.
The browser reaches the panel container through Docker's network, so requests never come from loopback.
`PANEL_ALLOW_REMOTE_CONTROL=1` allows this.
Do not set this variable when you use physical devices.

### Run the panel on your computer with the mocks

You can also run the panel outside Docker and connect it to the mock containers:

```sh
RUTM50_HOST=localhost:8441 RUTM50_PASSWORD=mock-password \
TSW202_HOST=localhost:8442 TSW202_PASSWORD=mock-password \
TSW202_2_HOST=localhost:8443 PORT=8090 \
cargo run -p teltonika-panel
```

A device host can include a port, as in `localhost:8441`.
Event Juggler events do not reach this panel, because the mocks send them to the panel container.

### Mock settings

Each mock reads these environment variables:

| Variable | Default | Meaning |
| --- | --- | --- |
| `MOCK_USERNAME` | `admin` | Login user name |
| `MOCK_PASSWORD` | `admin01` | Login password |
| `MOCK_TOKEN_TTL` | `299` | Token lifetime in seconds |
| `MOCK_PORT` | `443` | HTTPS port in the container |
| `MOCK_MAC` | `20:97:27:00:00:01` (RUTM50), `20:97:27:00:00:02` (TSW202) | Device MAC address |
| `MOCK_LAN_IP` | `192.168.30.1/24` (RUTM50), `192.168.30.2/24` (TSW202) | LAN address |
| `MOCK_WAN_IP` | `203.0.113.10/24` | RUTM50 WAN address |
| `MOCK_NEIGHBORS` | see the mock source | MAC addresses learned per port, as `port1=MAC,MAC;port3=MAC` |
| `MOCK_HOOK_URL` | none | Creates the `panel_*` Event Juggler rules at startup, with this URL |

To add a second switch mock, run another `tsw202` container.
Give it its own `MOCK_MAC` and `MOCK_LAN_IP`.
Set `MOCK_NEIGHBORS` on both switches so that each learns the other's MAC on the connecting port.

## Tests

The Rust unit tests check the API data rules: empty-list quirks, VLAN membership, MAC formats and link detection.

```sh
cargo test
```

The end-to-end tests run the Rust panel against the mock stack.
They check device reads, link detection, the status stream, pending changes, every on/off control, Event Juggler events and request checks.

```sh
npm install
npm run mock:up
npm test
```

Set `PANEL_URL` to test a panel at another address.
Do not point the tests at a panel that uses physical devices.
The tests turn ports, cellular data, Wi-Fi and Tailscale off and on.

Check the TypeScript of the mocks and tests:

```sh
npm run typecheck
```

## CI

GitHub Actions runs `.github/workflows/ci.yml` on Blacksmith runners for each push to `main` and each pull request:

- `rust` runs `cargo fmt`, `cargo clippy` and `cargo test` with Rust 1.91.0.
- `typecheck` checks the TypeScript of the mocks and tests.
- `mock-e2e` starts the mock stack and runs the end-to-end tests.

The Blacksmith GitHub App must have access to this repository.
Without it, the jobs wait in the queue and do not start.

## How the panel works

- Each device has its own poller in the server.
  A poller polls its device every 3 seconds with one `POST /api/bulk` request.
  It retries a device that does not answer every 30 seconds.
  A slow device does not delay the other devices.
- The pollers run only while someone uses the panel.
  They stop one minute after the last page closes.
- The client sends requests to a device one at a time, because the device web server stalls on parallel requests.
- The client caches data by how often it changes:
  - Device information: 10 minutes.
  - Configuration: 60 seconds.
  - Switch MAC table: 15 seconds.
  - Live status: every poll.
- The server keeps all panel state:
  - Changes that the device has not applied yet.
  - Ports that carry the panel's own connection.
  - The last control error.
  - Device events from Event Juggler.
- The server sends the full status to open pages through Server-Sent Events on `/api/events`.
  The page only draws what it gets.
- The Teltonika API does not push data.
  Event Juggler rules post to `/api/hook` instead, and the panel polls the devices again at once.
- The panel finds cables between devices from the MAC addresses each port learns.

## Project layout

| Path | Contents |
| --- | --- |
| `crates/teltonika/src/client.rs` | Teltonika API client: login, bulk requests, cache |
| `crates/teltonika/src/devices.rs` | Reads router and switch data into one model |
| `crates/teltonika/src/model.rs` | Status types |
| `crates/teltonika/src/control.rs` | On/off controls |
| `crates/teltonika/src/juggler.rs` | Event Juggler rules |
| `crates/teltonika/src/topology.rs` | Finds links between devices |
| `crates/teltonika-panel/src/main.rs` | Command line and settings |
| `crates/teltonika-panel/src/server.rs` | Web server, pollers, panel state, hooks and controls |
| `public/` | Web page: HTML, CSS and browser code |
| `mocks/` | Mock RUTM50 and TSW202 containers |
| `compose.mock.yaml` | Mock stack |
| `test/` | End-to-end tests |

## Limitations

- The panel reads PoE power as `budget` / 1000 W.
  This conversion is not verified on a switch with a PoE load.
- The panel was not tested with two physical switches.
  The mocks can simulate this setup.
- The panel has no login.
  Run it only on a trusted network.
