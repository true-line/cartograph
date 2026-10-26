import axios from "axios";

const API = "https://api.example.com";

export function listItems() {
  return fetch(`${API}/items`);
}

export function getItem(id: string) {
  return fetch(API + "/items/" + id);
}

export function search(q: string) {
  return axios.get("https://search.example.com/v1?q=" + q);
}

export function health() {
  return fetch(`${import.meta.env.VITE_HEALTH_URL}/health`);
}

export function follow(target: string) {
  return axios.get(target);
}
