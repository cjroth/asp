/* ====================================================================
   persist.ts · OPFS persistence for the demo mesh
   --------------------------------------------------------------------
   Saves the whole network (per-node seed + vault id + all wire rows +
   topology + tweaks) to the Origin Private File System so the mesh
   survives a reload. Restore replays the rows through the real engine
   (network.restore → WasmEngine.integrate), so what comes back is the
   real fold, not a snapshot of rendered state.

   Feature-detected: if OPFS or main-thread writable files are
   unavailable (e.g. Safari needs a worker), it degrades to a clean
   per-session demo. Chromium/Firefox support the main-thread path used
   here.
   ==================================================================== */
const FILE = 'asp-demo-state.json';

async function dir(): Promise<any | null> {
  try {
    const storage: any = (navigator as any)?.storage;
    if (!storage?.getDirectory) return null;
    return await storage.getDirectory();
  } catch {
    return null;
  }
}

export async function loadState(): Promise<any | null> {
  try {
    const d = await dir();
    if (!d) return null;
    const fh = await d.getFileHandle(FILE).catch(() => null);
    if (!fh) return null;
    const file = await fh.getFile();
    const txt = await file.text();
    return txt ? JSON.parse(txt) : null;
  } catch {
    return null;
  }
}

export async function saveState(state: unknown): Promise<void> {
  try {
    const d = await dir();
    if (!d) return;
    const fh = await d.getFileHandle(FILE, { create: true });
    if (!fh.createWritable) return; // Safari main-thread: no writable; skip
    const w = await fh.createWritable();
    await w.write(JSON.stringify(state));
    await w.close();
  } catch {
    /* best-effort */
  }
}

export async function clearState(): Promise<void> {
  try {
    const d = await dir();
    await d?.removeEntry?.(FILE);
  } catch {
    /* ignore */
  }
}

export function opfsAvailable(): boolean {
  try {
    return !!(navigator as any)?.storage?.getDirectory;
  } catch {
    return false;
  }
}
