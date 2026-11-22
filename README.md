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

- Node.js 23.6 or later. The panel uses Node 25 in Docker and CI.
- Docker with Compose, for the mock devices only.

The panel has no runtime dependencies.
Node runs the TypeScript files directly.

## Use physical devices

1. Install the development tools:

   ```sh
   npm install
   ```

2. Create a file named `.env` in the project root:

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

3. Start the panel:

   ```sh
   PORT=8080 npm start
   ```

4. Open http://localhost:8080.

5. Create the Event Juggler rules on the devices:

   ```sh
   npm run setup-hooks
   ```

   This step is optional.
   Without the rules, the panel still polls the devices every few seconds.
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

This builds and starts three containers: the two mocks and the panel.
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
PORT=8090 node src/server.ts
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

The end-to-end tests run the panel against the mock stack.
They check device reads, link detection, every on/off control, Event Juggler events and request checks.

```sh
npm run mock:up
npm test
```

Set `PANEL_URL` to test a panel at another address.
Do not point the tests at a panel that uses physical devices.
The tests turn ports, cellular data, Wi-Fi and Tailscale off and on.

Check the types:

```sh
npm run typecheck
```

## CI

GitHub Actions runs `.github/workflows/ci.yml` on Blacksmith runners for each push to `main` and each pull request:

- `typecheck` runs the TypeScript compiler.
- `mock-e2e` starts the mock stack and runs the end-to-end tests.

The Blacksmith GitHub App must have access to this repository.
Without it, the jobs wait in the queue and do not start.

## How the panel works

- The server polls each device with one `POST /api/bulk` request.
  It sends requests to a device one at a time, because the device web server stalls on parallel requests.
- The server caches data by how often it changes:
  - Device information: 10 minutes.
  - Configuration: 60 seconds.
  - Switch MAC table: 15 seconds.
  - Live status: every poll.
- The server keeps results for 3 seconds and shares them between all viewers.
  It tries a device that does not answer again after 30 seconds.
- The Teltonika API does not push data.
  Event Juggler rules post to `/api/hook` instead, and the panel refreshes at once.
- The server tells open pages about new data through Server-Sent Events on `/api/events`.
- The panel finds cables between devices from the MAC addresses each port learns.

## Project layout

| Path | Contents |
| --- | --- |
| `src/server.ts` | HTTP server, pollers, hooks and device control |
| `src/teltonika.ts` | Teltonika API client |
| `src/devices.ts` | Reads router and switch data into one model |
| `src/topology.ts` | Finds links between devices |
| `src/setup-hooks.ts` | Creates the Event Juggler rules |
| `src/client/app.ts` | Browser code |
| `public/` | HTML and CSS |
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
