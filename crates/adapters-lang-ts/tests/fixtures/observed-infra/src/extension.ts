import browser from "webextension-polyfill";

export function saveDraft(draft: string) {
  return chrome.storage.local.set({ draft });
}

export function loadPrefs() {
  return browser.storage.sync.get("prefs");
}
