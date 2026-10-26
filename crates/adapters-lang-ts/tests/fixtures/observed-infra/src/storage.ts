export function openInventory() {
  return indexedDB.open("inventory", 2);
}

export function rememberTab(tab: string) {
  localStorage.setItem("tab", tab);
}
