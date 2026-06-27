// Are we running inside the Tauri desktop shell, or as a plain web app? On
// desktop the backend is the native engine (Tauri commands); on web it's the
// asp-core engine compiled to wasm with OPFS persistence (see webApi.ts).
export function isDesktop(): boolean {
  return typeof window !== 'undefined' && !!((window as unknown as Record<string, unknown>).__TAURI__ || (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__);
}
