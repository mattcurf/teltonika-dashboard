// Finds the cables between panel devices from the MAC addresses each port has learned.
import type { DeviceBase, DeviceLink, PortStatus } from "./types.ts";

type Node = Pick<DeviceBase, "online" | "mac" | "ports"> & { label: string };

const portToward = (from: Node, to: Node): PortStatus | undefined =>
  to.mac ? from.ports.find((p) => p.macs.includes(to.mac!)) : undefined;

/**
 * Devices A and B share a cable when A learned B's MAC on port pa and B learned A's MAC on
 * port pb, and no third device was learned on both pa and pb. A switch between them would
 * show up on both ports, so router -> #1 -> #2 gives two links, not three.
 */
export function findLinks(nodes: Node[]): DeviceLink[] {
  const up = nodes.filter((n) => n.online && n.mac);
  const links: DeviceLink[] = [];
  for (let i = 0; i < up.length; i++) {
    for (let j = i + 1; j < up.length; j++) {
      const [a, b] = [up[i], up[j]];
      const pa = portToward(a, b);
      const pb = portToward(b, a);
      if (!pa || !pb) continue;
      const between = up.some((c) => c !== a && c !== b && pa.macs.includes(c.mac!) && pb.macs.includes(c.mac!));
      if (between) continue;
      links.push({
        a: { device: a.label, port: pa.label },
        b: { device: b.label, port: pb.label },
        up: pa.linkUp,
        speedMbps: pa.speedMbps,
        vlan: pa.vlan,
      });
    }
  }
  return links;
}
