export function openInventory() {
  return indexedDB.open("inventory", 1);
}
