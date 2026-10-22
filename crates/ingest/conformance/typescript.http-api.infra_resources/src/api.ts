const API = "https://api.example.com";

export function loadItems() {
  return fetch(`${API}/items`);
}
