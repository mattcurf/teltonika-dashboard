// Browser client. It only renders: the server sends the full status, including pending
// changes, locked ports and the last control error, and pushes a new one on every change.
const app = document.getElementById("app");
const diagram = document.getElementById("diagram");
const updated = document.getElementById("updated");
const pulse = document.getElementById("pulse");
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
function bytes(n) {
    const u = ["B", "KB", "MB", "GB", "TB"];
    let i = 0;
    while (n >= 1024 && i < u.length - 1) {
        n /= 1024;
        i++;
    }
    return `${n.toFixed(i ? 1 : 0)} ${u[i]}`;
}
const rate = (bps) => `${bytes(bps)}/s`;
const speed = (mbps) => (mbps >= 1000 ? `${mbps / 1000} Gbps` : `${mbps} Mbps`);
function uptime(s) {
    const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
    return d ? `${d}d ${h}h` : h ? `${h}h ${m}m` : `${m}m`;
}
function stat(k, v, pct) {
    const bar = pct === undefined ? "" : `<div class="bar"><i style="width:${Math.min(100, pct).toFixed(0)}%"></i></div>`;
    return `<div class="stat"><div class="k">${k}</div><div class="v">${esc(v)}</div>${bar}</div>`;
}
function systemStats(s) {
    return `<div class="stats">
    ${stat("Uptime", uptime(s.uptimeSeconds))}
    ${stat("Load (1m)", s.load1.toFixed(2))}
    ${stat("RAM", `${s.ramPercent.toFixed(0)}%`, s.ramPercent)}
    ${stat("Flash", `${s.flashPercent.toFixed(0)}%`, s.flashPercent)}
  </div>`;
}
function head(d, title, extra = "") {
    const s = d.system;
    const badge = d.online
        ? `<span class="badge ok">Online</span>`
        : d.error === "Connecting…"
            ? `<span class="badge warn">Connecting</span>`
            : `<span class="badge bad">Unreachable</span>`;
    const meta = s ? `${esc(s.hostname)} · ${esc(d.host)} · FW ${esc(s.firmware)} · S/N ${esc(s.serial)}` : esc(d.host);
    return `<div class="device-head">
    <div><h2>${esc(title)}</h2><div class="meta">${meta}${extra}</div></div>
    ${badge}
  </div>`;
}
// Signal grading follows common LTE/NR thresholds.
function grade(kind, v) {
    if (v === null)
        return "";
    const t = { rsrp: [-90, -105], rsrq: [-10, -15], sinr: [13, 0], rssi: [-70, -85] }[kind];
    return v >= t[0] ? "good" : v >= t[1] ? "fair" : "poor";
}
function bars(rsrp) {
    const n = rsrp === null ? 0 : rsrp >= -85 ? 4 : rsrp >= -95 ? 3 : rsrp >= -105 ? 2 : rsrp >= -115 ? 1 : 0;
    const cls = n >= 3 ? "" : n === 2 ? "mid" : "low";
    return `<div class="signal ${cls}">${[1, 2, 3, 4].map((i) => `<i class="${i <= n ? "on" : ""}"></i>`).join("")}</div>`;
}
function metric(k, v, unit, kind) {
    return `<div class="metric"><div class="k">${k}</div><div class="v ${grade(kind, v)}">${v === null ? "—" : `${v}<small> ${unit}</small>`}</div></div>`;
}
const toggleNames = { cellular: "Cellular", wifi: "Wi-Fi", tailscale: "Tailscale" };
const offWarnings = {
    cellular: "Turn off cellular data on the RUTM50? The modem stays registered, but the mobile WAN goes down.",
    wifi: "Turn off both Wi-Fi radios on the RUTM50? All Wi-Fi clients will disconnect.",
    tailscale: "Turn off Tailscale on the RUTM50? The router leaves the tailnet until you turn it back on.",
};
// Slider for a router feature. `control` comes from the server: {on, pending}, or null when
// the router does not have the feature.
function toggle(target, control) {
    if (!control)
        return "";
    const label = control.pending ? "Applying…" : `${toggleNames[target]} ${control.on ? "on" : "off"}`;
    return `<label class="toggle">
    <input type="checkbox" data-toggle="${target}" ${control.on ? "checked" : ""} ${control.pending ? "disabled" : ""} />
    <span class="track"><span class="thumb"></span></span><span class="toggle-label">${label}</span>
  </label>`;
}
function cellular(c) {
    const badge = !c.dataEnabled
        ? `<span class="badge off">Data disabled</span>`
        : c.dataConnected
            ? `<span class="badge ok">Data connected</span>`
            : `<span class="badge warn">${esc(c.state)}</span>`;
    const carriers = c.carriers
        .map((ca) => `<span class="chip ${ca.primary ? "" : "secondary"}">${esc(ca.band)} · ${esc(ca.bandwidthMHz)} MHz${ca.primary ? " (P)" : ""}</span>`)
        .join("");
    return `<div class="card">
    <div class="cell">
      ${bars(c.rsrp)}
      <div>
        <div class="wifi-head"><div class="cell-title">${esc(c.operator)} · ${esc(c.connectionType)}</div>${badge}</div>
        <div class="cell-sub">${esc(c.band)} · SIM ${c.activeSim} ${esc(c.simState.toLowerCase())} · ${esc(c.modemModel)}</div>
      </div>
    </div>
    <div class="metrics">
      ${metric("RSRP", c.rsrp, "dBm", "rsrp")}
      ${metric("RSRQ", c.rsrq, "dB", "rsrq")}
      ${metric("SINR", c.sinr, "dB", "sinr")}
      ${metric("RSSI", c.rssi, "dBm", "rssi")}
    </div>
    ${carriers ? `<div class="chips">${carriers}</div>` : ""}
    <dl class="kv">
      <dt>Mobile IP</dt><dd>${esc(c.ipv4.join(", ") || "—")}</dd>
      <dt>Data (session)</dt><dd>↓ ${bytes(c.rxBytes)} · ↑ ${bytes(c.txBytes)}</dd>
      <dt>Modem temp</dt><dd>${c.temperatureC === null ? "—" : `${c.temperatureC} °C`}</dd>
    </dl>
  </div>`;
}
function wifi(w) {
    const badge = w.up ? `<span class="badge ok">${esc(w.mode.toUpperCase())} up</span>` : `<span class="badge off">Down</span>`;
    const clients = w.clients.length
        ? `<ul class="clients">${w.clients
            .map((c) => `<li><span>${esc(c.hostname || c.mac)} <span class="muted">${esc(c.ip)}</span></span><span class="muted">${esc(c.signal)} · ${esc(c.standard)}</span></li>`)
            .join("")}</ul>`
        : `<div class="empty" style="margin-top:10px">No clients</div>`;
    return `<div class="card">
    <div class="wifi-head"><span class="ssid">${esc(w.ssid)}</span>${badge}</div>
    <div class="chips">
      <span class="chip">${esc(w.band)}</span>
      <span class="chip secondary">Ch ${w.channel}</span>
      <span class="chip secondary">${w.txPowerDbm} dBm</span>
      <span class="chip secondary">${esc(w.encryption)}</span>
    </div>
    <dl class="kv"><dt>Clients</dt><dd>${w.clients.length}</dd><dt>Noise floor</dt><dd>${w.noiseDbm} dBm</dd></dl>
    ${clients}
  </div>`;
}
function tailscale(t) {
    const badge = !t.enabled
        ? `<span class="badge off">Disabled</span>`
        : t.loginUrl
            ? `<span class="badge warn">Login required</span>`
            : t.running
                ? `<span class="badge ok">Connected</span>`
                : `<span class="badge bad">Stopped</span>`;
    const v4 = t.addresses.filter((a) => !a.includes(":"));
    const v6 = t.addresses.filter((a) => a.includes(":"));
    const pct = t.peersTotal ? (t.peersOnline / t.peersTotal) * 100 : 0;
    const login = t.loginUrl ? `<div class="chips"><a class="chip" href="${esc(t.loginUrl)}" target="_blank" rel="noopener">Open login URL</a></div>` : "";
    const msgs = t.messages.length ? `<div class="empty" style="margin-top:10px">${t.messages.map(esc).join("<br>")}</div>` : "";
    return `<div class="card">
    <div class="wifi-head">
      <div><div class="cell-title mono">${esc(v4.join(", ") || "No tailnet address")}</div>
      <div class="cell-sub mono">${esc(v6.join(", "))}</div></div>
      ${badge}
    </div>
    <div class="stats ts-stats">
      <div class="stat"><div class="k">Peers online</div><div class="v">${t.peersOnline} / ${t.peersTotal}</div><div class="bar"><i style="width:${pct.toFixed(0)}%;background:var(--ok)"></i></div></div>
      <div class="stat"><div class="k">Version</div><div class="v">${esc(t.version || "—")}</div></div>
      <div class="stat"><div class="k">Memory</div><div class="v">${bytes(t.memoryBytes)}</div></div>
      <div class="stat"><div class="k">Auth</div><div class="v">${esc(t.authType === "url" ? "Login URL" : t.authType === "key" ? "Auth key" : t.authType || "—")}</div></div>
    </div>
    ${login}${msgs}
  </div>`;
}
// Each jack is a button that turns the port on or off. Ports that carry the panel's own
// connection are locked on, because turning them off would cut the panel off from the device.
const portJack = (device) => (p) => {
    // The server sets `pending` while the device applies a change, and `locked` on ports that
    // carry the panel's own connection.
    const busy = p.pending !== null && p.pending !== undefined;
    const locked = p.locked;
    const cls = ["port", p.linkUp ? "up" : "", p.linkUp && p.speedMbps < 1000 ? "s100" : "", p.enabled ? "" : "disabled", busy ? "busy" : "", locked ? "locked" : ""].join(" ");
    const action = locked ? "Locked on: carries this panel's connection" : busy ? "Applying…" : `Click to turn ${p.enabled ? "off" : "on"}`;
    const tip = `${p.label}: ${p.linkUp ? `${speed(p.speedMbps)} ${p.fullDuplex ? "full" : "half"} duplex` : p.enabled ? "no link" : "disabled"}` +
        `\nDefault VLAN: ${p.vlan.untagged ?? "none"}\nTrunked VLANs: ${p.vlan.tagged.join(", ") || "none"}\n${action}`;
    const poe = p.poe?.active ? `<span class="poe">⚡ ${p.poe.watts.toFixed(1)} W</span>` : "";
    return `<button type="button" class="${cls}" title="${esc(tip)}" data-device="${esc(device)}" data-port="${esc(p.id)}" data-label="${esc(p.label)}" data-enabled="${p.enabled}" ${busy ? "disabled" : ""}>
    <div class="jack ${p.kind === "sfp" ? "sfp" : ""}">${locked ? `<span class="lock">🔒</span>` : ""}</div>
    <span class="name">${esc(p.label)}</span>
    <span>${busy ? "…" : p.linkUp ? speed(p.speedMbps) : p.enabled ? "—" : "off"}</span>${poe}
  </button>`;
};
const legend = `<span class="legend"><span><i class="dot ok"></i>1 Gbps</span><span><i class="dot warn"></i>10/100 Mbps</span><span><i class="dot"></i>No link</span></span>`;
function linkCell(p) {
    if (!p.enabled)
        return `<i class="dot"></i>Disabled`;
    if (!p.linkUp)
        return `<i class="dot"></i>Down`;
    return `<i class="dot ${p.speedMbps >= 1000 ? "ok" : "warn"}"></i>${speed(p.speedMbps)} ${p.fullDuplex ? "FD" : "HD"}`;
}
// The router WAN port sits outside the LAN bridge, so it has no bridge VLAN.
function vlanCells(p) {
    const def = p.vlan.untagged !== null ? `<span class="chip">VLAN ${p.vlan.untagged}</span>` : p.kind === "wan" ? `<span class="muted">Routed</span>` : "—";
    const trunk = p.vlan.tagged.length ? p.vlan.tagged.map((v) => `<span class="chip secondary">${v}</span>`).join(" ") : "—";
    return `<td>${def}</td><td><span class="vlans">${trunk}</span></td>`;
}
function routerPorts(ports) {
    const lan = ports.filter((p) => p.kind === "lan");
    const wan = ports.filter((p) => p.kind === "wan");
    const rows = ports
        .map((p) => `<tr class="${p.linkUp ? "" : "down"}"><td>${esc(p.label)}</td><td>${linkCell(p)}</td>${vlanCells(p)}<td>${p.linkUp ? p.neighbors ?? 0 : "—"}</td></tr>`)
        .join("");
    return `<div class="section">
    <h3>Ethernet ports ${legend}</h3>
    <div class="faceplate">
      <div class="group single">${lan.map(portJack("RUTM50")).join("")}</div>
      <div class="sep"></div>
      <div class="group single">${wan.map(portJack("RUTM50")).join("")}</div>
    </div>
    <table class="ports"><thead><tr><th>Port</th><th>Link</th><th>Default VLAN</th><th>Trunked VLANs</th><th>Learned MACs</th></tr></thead><tbody>${rows}</tbody></table>
  </div>`;
}
function switchPorts(device, ports) {
    const eth = ports.filter((p) => p.kind !== "sfp");
    const sfp = ports.filter((p) => p.kind === "sfp");
    // Match the faceplate: the grid fills column by column, so interleave top-row and bottom-row ports.
    const ordered = (list) => {
        const top = list.filter((p) => p.row === "up");
        const bottom = list.filter((p) => p.row === "down");
        return top.flatMap((p, i) => (bottom[i] ? [p, bottom[i]] : [p]));
    };
    const totalPoe = eth.reduce((sum, p) => sum + (p.poe?.active ? p.poe.watts : 0), 0);
    const up = ports.filter((p) => p.linkUp).length;
    const rows = ports
        .map((p) => `<tr class="${p.linkUp ? "" : "down"}">
        <td>${esc(p.kind === "sfp" ? p.label : `Port ${p.label}`)}</td>
        <td>${linkCell(p)}</td>
        ${vlanCells(p)}
        <td>${p.poe ? (p.poe.active ? `${p.poe.watts.toFixed(1)} W` : p.poe.enabled ? "On" : "Off") : "—"}</td>
        <td>${p.linkUp || p.rxBytes ? `↓ ${rate(p.rxRate ?? 0)} · ↑ ${rate(p.txRate ?? 0)}<div class="muted small">Σ ↓ ${bytes(p.rxBytes ?? 0)} · ↑ ${bytes(p.txBytes ?? 0)}</div>` : "—"}</td>
      </tr>`)
        .join("");
    return `<div class="section">
    <h3>Ports · ${up}/${ports.length} linked · PoE ${totalPoe.toFixed(1)} W ${legend}</h3>
    <div class="faceplate">
      <div class="group">${ordered(eth).map(portJack(device)).join("")}</div>
      <div class="sep"></div>
      <div class="group">${ordered(sfp).map(portJack(device)).join("")}</div>
    </div>
    <table class="ports"><thead><tr><th>Port</th><th>Link</th><th>Default VLAN</th><th>Trunked VLANs</th><th>PoE</th><th>Traffic (rate / total)</th></tr></thead><tbody>${rows}</tbody></table>
  </div>`;
}
function renderRouter(r, controls, notice) {
    if (!r.online)
        return `<section class="device">${head(r, r.system?.model ?? "RUTM50")}<div class="error">${esc(r.error)}</div></section>`;
    const wan = r.wanIpv4.length ? ` · WAN ${esc(r.wanIpv4.join(", "))}` : "";
    return `<section class="device">
    ${head(r, r.system?.model ?? "RUTM50", wan)}
    ${systemStats(r.system)}
    ${notice ? `<div class="error">${esc(notice)}</div>` : ""}
    <div class="section"><h3>Cellular ${toggle("cellular", controls.cellular)}</h3>
      ${r.cellular.length ? r.cellular.map(cellular).join("") : `<div class="empty">No modem</div>`}</div>
    <div class="section"><h3>Wi-Fi ${toggle("wifi", controls.wifi)}</h3><div class="cards">${r.wifi.length ? r.wifi.map(wifi).join("") : `<div class="empty">No radios</div>`}</div></div>
    ${r.tailscale ? `<div class="section"><h3>Tailscale ${toggle("tailscale", controls.tailscale)}</h3>${tailscale(r.tailscale)}</div>` : ""}
    ${routerPorts(r.ports)}
  </section>`;
}
function renderSwitch(s) {
    if (!s.online)
        return `<section class="device">${head(s, s.label)}<div class="error">${esc(s.error)}</div><div class="empty">The panel checks again every 30 s and shows the switch once it answers.</div></section>`;
    return `<section class="device">
    ${head(s, s.label)}
    ${systemStats(s.system)}
    ${switchPorts(s.label, s.ports)}
  </section>`;
}
// ---- Logical network diagram (SVG, HMI style) ----
// Fixed layout in a 1000-wide viewBox: uplinks on top, the router in the middle, switches below.
const DW = 1000;
const bottom = (b, dx = 0) => ({ x: b.x + dx, y: b.y + b.h / 2 });
const top = (b, dx = 0) => ({ x: b.x + dx, y: b.y - b.h / 2 });
function vlanText(v) {
    const parts = [];
    if (v.untagged !== null)
        parts.push(`VLAN ${v.untagged} U`);
    if (v.tagged.length)
        parts.push(`T ${v.tagged.join(",")}`);
    return parts.join(" · ") || "no VLAN";
}
// Text with a dark backing plate, so it stays readable on top of lines.
function tag(x, y, lines, cls = "") {
    const w = Math.max(...lines.map((l) => l.length)) * 6.4 + 14;
    const h = lines.length * 14 + 6;
    return `<g class="dg-tag ${cls}"><rect x="${x - w / 2}" y="${y - h / 2}" width="${w}" height="${h}" rx="5"/>${lines
        .map((l, i) => `<text x="${x}" y="${y - h / 2 + 15 + i * 14}">${esc(l)}</text>`)
        .join("")}</g>`;
}
// Port badge where a wire meets a box. dy moves it off the box edge: negative above, positive below.
function portChip(x, y, dy, port, state) {
    const w = port.length * 6.2 + 12;
    const cy = y + dy;
    return `<g class="dg-port ${state}"><rect x="${x - w / 2}" y="${cy - 8}" width="${w}" height="16" rx="4"/><text x="${x}" y="${cy + 3.5}">${esc(port)}</text></g>`;
}
const portName = (end) => (end.device === "RUTM50" ? end.port : `Port ${end.port}`);
// state: ok (green), warn (amber), off (gray), bad (red); dashed for links that are down or absent.
// Node height grows with its text lines; anchors for wires use the same height.
const nodeHeight = (lines) => 34 + lines * 16;
function node(b, title, lines, state, dashed = false) {
    return `<g class="dg-node ${state}${dashed ? " dashed" : ""}">
    <rect x="${b.x - b.w / 2}" y="${b.y - b.h / 2}" width="${b.w}" height="${b.h}" rx="10"/>
    <circle cx="${b.x - b.w / 2 + 14}" cy="${b.y - b.h / 2 + 15}" r="4"/>
    <text class="dg-title" x="${b.x - b.w / 2 + 26}" y="${b.y - b.h / 2 + 19}">${esc(title)}</text>
    ${lines.map((l, i) => `<text class="dg-sub" x="${b.x - b.w / 2 + 14}" y="${b.y - b.h / 2 + 38 + i * 15}">${esc(l)}</text>`).join("")}
  </g>`;
}
function wire(d, state, style = "") {
    return `<path class="dg-wire ${state} ${style}" d="${d}"/>`;
}
function renderDiagram(data) {
    const r = data.router;
    const ipLines = (d) => (d.lanIpv4?.length ? d.lanIpv4 : [d.host]);
    const routerLines = [...ipLines(r).map((ip) => `LAN ${ip}`), r.online ? "gateway · DHCP · VLAN routing" : (r.error ?? "unreachable")];
    const switchLines = data.switches.map((s) => {
        const linked = data.links.some((l) => l.a.device === s.label || l.b.device === s.label);
        const portsUp = s.ports.filter((p) => p.linkUp).length;
        return s.online
            ? [...ipLines(s), `${portsUp}/${s.ports.length} ports up${linked ? "" : " · uplink not found"}`]
            : [s.host, s.error === "Connecting…" ? "connecting…" : "not detected"];
    });
    const routerBox = { x: 500, y: 170, w: 260, h: nodeHeight(routerLines.length) };
    const n = data.switches.length;
    const switchBoxes = data.switches.map((_, i) => {
        const h = nodeHeight(switchLines[i].length);
        return { x: (DW * (i + 1)) / (n + 1), y: 330 + h / 2 - 37, w: 240, h };
    });
    const parts = [];
    // Uplinks above the router.
    const wanPort = r.ports.find((p) => p.kind === "wan");
    const cell = r.cellular[0];
    const ts = r.tailscale;
    const uplinks = [
        {
            box: { x: 170, y: 42, w: 210, h: 50 },
            title: "Upstream / WAN",
            sub: r.wanIpv4.join(", ") || "no address",
            state: wanPort?.linkUp ? "ok" : "bad",
            label: [`WAN · ${wanPort?.linkUp ? speed(wanPort.speedMbps) : "down"}`],
            port: wanPort?.label ?? "WAN",
            style: "",
        },
        cell && {
            box: { x: 500, y: 42, w: 210, h: 50 },
            title: `Cellular · ${cell.operator}`,
            sub: !cell.dataEnabled ? "data disabled" : cell.dataConnected ? cell.ipv4.join(", ") || "connected" : cell.state,
            state: !cell.dataEnabled ? "off" : cell.dataConnected ? "ok" : "warn",
            label: [`${cell.connectionType} · ${cell.band}`],
            port: `SIM${cell.activeSim}`,
            style: cell.dataConnected ? "" : "dashed",
        },
        ts && {
            box: { x: 830, y: 42, w: 210, h: 50 },
            title: "Tailnet",
            sub: ts.addresses.find((a) => !a.includes(":")) ?? (ts.enabled ? "no address" : "disabled"),
            state: !ts.enabled ? "off" : ts.running && !ts.loginUrl ? "ok" : "warn",
            label: [`overlay · ${ts.peersOnline}/${ts.peersTotal} peers`],
            port: "tailscale0",
            style: "dotted",
        },
    ].filter((u) => !!u);
    uplinks.forEach((u, i) => {
        const from = bottom(u.box);
        const to = top(routerBox, (i - (uplinks.length - 1) / 2) * 60);
        const midY = (from.y + to.y) / 2 + 4;
        parts.push(wire(`M${from.x},${from.y} V${midY} H${to.x} V${to.y}`, u.state, u.style));
        parts.push(node(u.box, u.title, [u.sub], u.state));
        // Label just under its uplink box, on the vertical drop.
        parts.push(tag(from.x, from.y + 18, u.label, u.state));
        parts.push(portChip(to.x, to.y, -11, u.port, u.state));
    });
    // Wi-Fi as a wireless bubble beside the router.
    if (r.wifi.length || r.wifiControl) {
        const on = r.wifiControl?.enabled ?? r.wifi.some((w) => w.up);
        const clients = r.wifi.reduce((s, w) => s + w.clients.length, 0);
        // One line per broadcasting SSID, then the client count.
        const ssids = r.wifi.filter((w) => w.up).map((w) => `${w.ssid} · ${w.band}`);
        const wifiLines = on ? [...(ssids.length ? ssids : ["no SSIDs up"]), `${clients} client${clients === 1 ? "" : "s"}`] : ["radios off"];
        const wifiBox = { x: 150, y: 170, w: 220, h: nodeHeight(wifiLines.length) };
        const state = on ? "ok" : "off";
        parts.push(wire(`M${wifiBox.x + wifiBox.w / 2},${wifiBox.y} H${routerBox.x - routerBox.w / 2}`, state, "dotted"));
        parts.push(node(wifiBox, "Wi-Fi", wifiLines, state));
        parts.push(tag((wifiBox.x + wifiBox.w / 2 + routerBox.x - routerBox.w / 2) / 2, wifiBox.y, ["bridged to LAN"], state));
        parts.push(portChip(routerBox.x - routerBox.w / 2 - 22, wifiBox.y, -21, "WLAN", state));
    }
    // Cables between devices, found from learned MAC addresses.
    let lowest = Math.max(...switchBoxes.map((b) => b.y + b.h / 2), routerBox.y + routerBox.h / 2) + 16;
    const boxOf = new Map([["RUTM50", routerBox], ...data.switches.map((s, i) => [s.label, switchBoxes[i]])]);
    const routerLinks = data.links.filter((l) => l.a.device === "RUTM50" || l.b.device === "RUTM50");
    data.links.forEach((l) => {
        const state = l.up ? (l.speedMbps >= 1000 ? "ok" : "warn") : "off";
        // Port numbers are drawn as chips at each end, so the label carries VLAN and speed only.
        const label = [vlanText(l.vlan), l.up ? speed(l.speedMbps) : "down"];
        const endOf = (b) => (b === boxOf.get(l.a.device) ? l.a : l.b);
        const [ra, rb] = [boxOf.get(l.a.device), boxOf.get(l.b.device)];
        if (ra === routerBox || rb === routerBox) {
            const sw = ra === routerBox ? rb : ra;
            const i = routerLinks.indexOf(l);
            const from = bottom(routerBox, (i - (routerLinks.length - 1) / 2) * 50);
            const to = top(sw);
            const midY = from.y + 20;
            parts.push(wire(`M${from.x},${from.y} V${midY} H${to.x} V${to.y}`, state, l.up ? "" : "dashed"));
            // Beside the vertical drop, on the side away from the router.
            const side = to.x < routerBox.x ? -1 : 1;
            const w = Math.max(...label.map((t) => t.length)) * 6.4 + 14;
            parts.push(tag(to.x + side * (w / 2 + 34), (midY + to.y) / 2 - 4, label, state));
            parts.push(portChip(from.x, from.y, 10, portName(endOf(routerBox)), state));
            parts.push(portChip(to.x, to.y, -11, portName(endOf(sw)), state));
        }
        else {
            // Switch to switch: a U under both boxes, so the label has room on the bottom run.
            const [left, right] = ra.x < rb.x ? [ra, rb] : [rb, ra];
            const y = Math.max(left.y + left.h / 2, right.y + right.h / 2) + 34;
            parts.push(wire(`M${left.x},${left.y + left.h / 2} V${y} H${right.x} V${right.y + right.h / 2}`, state, l.up ? "" : "dashed"));
            parts.push(tag((left.x + right.x) / 2, y, label, state));
            parts.push(portChip(left.x, left.y + left.h / 2, 11, portName(endOf(left)), state));
            parts.push(portChip(right.x, right.y + right.h / 2, 11, portName(endOf(right)), state));
            lowest = Math.max(lowest, y + 30);
        }
    });
    // Device nodes go last so they sit on top of the lines.
    parts.push(node(routerBox, "RUTM50", routerLines, r.online ? "ok" : "bad"));
    data.switches.forEach((s, i) => {
        const linked = data.links.some((l) => l.a.device === s.label || l.b.device === s.label);
        parts.push(node(switchBoxes[i], s.label, switchLines[i], s.online ? (linked ? "ok" : "warn") : "bad", !s.online));
    });
    const legend = `<div class="dg-legend">
    <span><i class="ok"></i>1 Gbps / up</span><span><i class="warn"></i>10/100 Mbps / degraded</span><span><i class="off"></i>off</span><span><i class="bad"></i>unreachable</span>
    <span><i class="line dashed"></i>down / backup</span><span><i class="line dotted"></i>wireless / overlay</span>
  </div>`;
    return `<div class="dg-head"><h3>Logical network</h3>${legend}</div>
    <div class="dg-scroll"><svg viewBox="0 0 ${DW} ${lowest}" role="img" aria-label="Logical network diagram">${parts.join("")}</svg></div>`;
}
function renderEvents(list) {
    const rows = list.length
        ? list
            .map((e) => `<li><span class="muted">${new Date(e.at).toLocaleTimeString()}</span><span class="chip ${e.device.startsWith("RUT") ? "" : "secondary"}">${esc(e.device)}</span><span>${esc(e.text || "(no text)")}</span></li>`)
            .join("")
        : `<li class="empty">No events since the panel started. Port link and mobile data changes appear here.</li>`;
    return `<section class="device events-panel"><div class="section"><h3>Device events · Event Juggler</h3><ul class="events">${rows}</ul></div></section>`;
}
function render(data) {
    // Switches stack in one column next to the router, in the panel's configured order.
    diagram.innerHTML = renderDiagram(data);
    app.innerHTML =
        renderRouter(data.router, data.controls, data.notice) + `<div class="column">${data.switches.map(renderSwitch).join("")}</div>` + renderEvents(data.events);
    updated.textContent = `Updated ${new Date(data.fetchedAt).toLocaleTimeString()}`;
    pulse.classList.remove("live");
    void pulse.offsetWidth;
    pulse.classList.add("live");
}
async function refresh() {
    try {
        const res = await fetch("/api/status", { cache: "no-store", redirect: "manual" });
        if (res.type === "opaqueredirect") {
            renewPortalSession();
            throw new Error("portal sign-in expired");
        }
        if (!res.ok)
            throw new Error(`HTTP ${res.status}`);
        render(await res.json());
    }
    catch (e) {
        updated.textContent = `Panel server unreachable (${e.message})`;
        pulse.classList.remove("live");
    }
}
// Amp's portal sign-in expires after about 10 minutes. The portal then answers POSTs with 401
// and redirects other requests to ampcode.com. Reloading the page signs in again; a confirmed
// change that hit the expiry is saved and sent once after the reload.
const RENEWED_AT = "panel-renewed-at";
const REPLAY = "panel-replay";
let reloading = false;
function renewPortalSession(replay) {
    // A reload that comes back still signed out would loop, so reload at most every 20 s.
    if (reloading || Date.now() - (Number(sessionStorage.getItem(RENEWED_AT)) || 0) < 20_000)
        return;
    reloading = true;
    sessionStorage.setItem(RENEWED_AT, String(Date.now()));
    if (replay)
        sessionStorage.setItem(REPLAY, JSON.stringify({ at: Date.now(), body: replay }));
    location.reload();
}
async function sendToggle(body) {
    const res = await fetch("/api/toggle", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
        redirect: "manual",
    });
    if (res.status === 401 || res.type === "opaqueredirect") {
        renewPortalSession(body);
        throw new Error("portal sign-in expired; reload the page and try again");
    }
    if (!res.ok)
        throw new Error((await res.json().catch(() => ({}))).error ?? `HTTP ${res.status}`);
}
// Port jacks, also by delegation.
app.addEventListener("click", async (ev) => {
    const btn = ev.target.closest("button.port");
    if (!btn || btn.disabled)
        return;
    const { device = "", port = "", label = "" } = btn.dataset;
    const name = /^\d+$/.test(label) ? `port ${label}` : label;
    if (btn.classList.contains("locked")) {
        alert(`${name} on ${device} carries this panel's own connection, so the panel keeps it on.`);
        return;
    }
    const enabled = btn.dataset.enabled !== "true";
    const question = enabled
        ? `Turn on ${name} on ${device}?`
        : `Turn off ${name} on ${device}? Anything connected to it loses its link until you turn it back on.`;
    if (!confirm(question))
        return;
    try {
        await sendToggle({ target: "port", device, port, enabled });
    }
    catch (e) {
        if (!reloading)
            alert(`Could not change ${name} on ${device}: ${e.message}`);
    }
});
// The page is re-rendered on every refresh, so handle the sliders by delegation.
app.addEventListener("change", async (ev) => {
    const input = ev.target;
    const target = input.dataset.toggle;
    if (!target)
        return;
    const enabled = input.checked;
    if (!enabled && !confirm(offWarnings[target])) {
        input.checked = true;
        return;
    }
    // The server shows the change as pending, and reports a failure in the status notice.
    await sendToggle({ target, enabled }).catch(() => { });
    void refresh();
});
// The server pushes the full status whenever it changes. EventSource reconnects by itself;
// on an error, refresh() finds out whether the server is down or the portal sign-in expired.
const events = new EventSource("/api/events");
events.onmessage = (e) => render(JSON.parse(e.data));
events.onerror = () => void refresh();
// Send a change that was confirmed just before the portal sign-in expired.
const replay = JSON.parse(sessionStorage.getItem(REPLAY) ?? "null");
sessionStorage.removeItem(REPLAY);
if (replay && Date.now() - replay.at < 60_000) {
    sendToggle(replay.body)
        .catch((e) => alert(`Could not apply the change: ${e.message}`))
        .finally(() => void refresh());
}
refresh();
