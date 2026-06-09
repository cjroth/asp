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
  const txt = await loadStateRaw();
  try {
    return txt ? JSON.parse(txt) : null;
  } catch {
    return null;
  }
}

/** The persisted state as its raw JSON text — handed straight to the engine
 * worker, which parses it once (off the main thread) rather than the main
 * thread parsing a multi-MB document and then structured-cloning the result
 * across the worker boundary. */
export async function loadStateRaw(): Promise<string | null> {
  try {
    const d = await dir();
    if (!d) return null;
    const fh = await d.getFileHandle(FILE).catch(() => null);
    if (!fh) return null;
    const file = await fh.getFile();
    const txt = await file.text();
    return txt || null;
  } catch {
    return null;
  }
}

export async function saveState(state: unknown): Promise<void> {
  await saveStateRaw(JSON.stringify(state));
}

/** Write an already-serialized JSON string. The engine worker builds this
 * string (the whole vault is in the worker), so the main thread never
 * `JSON.stringify`s the multi-MB document — it just streams it to disk. */
export async function saveStateRaw(json: string): Promise<void> {
  try {
    const d = await dir();
    if (!d) return;
    const fh = await d.getFileHandle(FILE, { create: true });
    if (!fh.createWritable) return; // Safari main-thread: no writable; skip
    const w = await fh.createWritable();
    await w.write(json);
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
