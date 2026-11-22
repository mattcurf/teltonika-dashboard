// Devices shown on the panel. Each switch gets its own card, stacked in this order.
import { TeltonikaClient } from "./teltonika.ts";

function env(name: string, fallback?: string): string {
  const v = process.env[name] ?? fallback;
  if (v === undefined) throw new Error(`Missing environment variable ${name}`);
  return v;
}

export const router = {
  label: "RUTM50",
  client: new TeltonikaClient(env("RUTM50_HOST", "192.168.30.1"), env("RUTM50_USERNAME", "admin"), env("RUTM50_PASSWORD")),
};

// The second switch uses the first switch's credentials unless TSW202_2_* overrides them.
export const switches = [
  {
    label: "TSW202 #1",
    client: new TeltonikaClient(env("TSW202_HOST", "192.168.30.2"), env("TSW202_USERNAME", "admin"), env("TSW202_PASSWORD")),
  },
  {
    label: "TSW202 #2",
    client: new TeltonikaClient(
      env("TSW202_2_HOST", "192.168.30.3"),
      env("TSW202_2_USERNAME", env("TSW202_USERNAME", "admin")),
      env("TSW202_2_PASSWORD", env("TSW202_PASSWORD")),
    ),
  },
];
