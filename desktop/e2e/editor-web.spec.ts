// Web-target e2e: the real editor UI in headless Chromium against the real wasm
// engine (iroh-in-wasm) + OPFS. No network: every assertion exercises the
// browser thin-node's file surface, persistence, history, and PITR — the sync
// path's correctness is proven by the Rust e2e suite (same engine, same Session)
// and the SDK parity tests; here we prove the *browser surface* end-to-end.
//
// Flow: New vault → (returns to the connect list) → click the vault row → editor.

import { expect, test } from '@playwright/test';

async function openFreshVault(page: import('@playwright/test').Page): Promise<void> {
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Your vaults' })).toBeVisible();
  await page.getByRole('button', { name: 'New vault' }).click();
  // The new vault appears in the list; click its row to open the editor.
  await expect(page.getByText('Browser vault').first()).toBeVisible();
  await page.getByText('Browser vault').first().click();
  await expect(page.getByRole('button', { name: '+ New' })).toBeVisible();
}

test.describe('Vault Editor — web target', () => {
  test.beforeEach(async ({ page }) => {
    await page.context().clearCookies();
  });

  test('creates a browser vault, opens it, writes a file', async ({ page }) => {
    await openFreshVault(page);
    await page.getByRole('button', { name: '+ New' }).click();
    // The new untitled file is selected; its heading renders in the live editor.
    await expect(page.getByText('untitled').first()).toBeVisible();
  });

  test('edits autosave and persist across reload (OPFS)', async ({ page }) => {
    await openFreshVault(page);
    await page.getByRole('button', { name: '+ New' }).click();
    const editor = page.locator('[contenteditable=true]');
    await editor.click();
    await editor.fill('');
    await page.keyboard.type('# Hello asp\n\nThis is a note.\n');
    await expect(page.getByText('Saved')).toBeVisible();
    await page.waitForTimeout(900); // debounce flush + worker persist
    await page.reload();
    // The vault row reappears (OPFS restored); open it and the content survives.
    await expect(page.getByText('Browser vault').first()).toBeVisible();
    await page.getByText('Browser vault').first().click();
    // The README is auto-selected; open the untitled file we edited.
    await page.locator('text=untitled.md').first().click();
    await expect(page.getByText('Hello asp')).toBeVisible();
  });

  test('rename and delete a file from the sidebar', async ({ page }) => {
    await openFreshVault(page);
    await page.getByRole('button', { name: '+ New' }).click();
    const fileRow = page.locator('text=untitled.md').first();
    await expect(fileRow).toBeVisible();
    // Right-click → Rename.
    await fileRow.click({ button: 'right' });
    await page.getByText('Rename').click();
    await page.keyboard.type('renamed.md');
    await page.keyboard.press('Enter');
    await expect(page.locator('text=renamed.md').first()).toBeVisible();
    // Right-click renamed → Delete.
    await page.locator('text=renamed.md').first().click({ button: 'right' });
    await page.getByText('Delete').click();
    await expect(page.getByText('Select a file or create a new one.')).toBeVisible();
  });

  test('history timeline renders events after edits', async ({ page }) => {
    await openFreshVault(page);
    await page.getByRole('button', { name: '+ New' }).click();
    const editor = page.locator('[contenteditable=true]');
    await editor.click();
    await page.keyboard.type('# v1\n');
    await expect(page.getByText('Saved')).toBeVisible();
    await page.waitForTimeout(900);
    // The timeline footer shows at least one row.
    await expect(page.getByText(/\d+ rows/)).toBeVisible();
  });

  test('remove-vault modal (browser: deletes the vault)', async ({ page }) => {
    await openFreshVault(page);
    await page.getByRole('button', { name: '‹ Back' }).click();
    const row = page.getByText('Browser vault').first();
    await row.click({ button: 'right' });
    await page.getByText('Remove vault').click();
    await expect(page.getByText(/This deletes the vault from this browser/)).toBeVisible();
    await page.getByRole('button', { name: 'Remove' }).click();
    // Back to the empty connect screen.
    await expect(page.getByRole('heading', { name: 'Your vaults' })).toBeVisible();
  });

  test('preview mode renders markdown', async ({ page }) => {
    await openFreshVault(page);
    await page.getByRole('button', { name: '+ New' }).click();
    const editor = page.locator('[contenteditable=true]');
    await editor.click();
    await page.keyboard.type('# A heading\n\n**bold** and `code`\n');
    await expect(page.getByText('Saved')).toBeVisible();
    await page.getByRole('button', { name: 'Preview' }).click();
    await expect(page.locator('h1', { hasText: 'A heading' })).toBeVisible();
    await expect(page.locator('strong', { hasText: 'bold' })).toBeVisible();
  });
});
