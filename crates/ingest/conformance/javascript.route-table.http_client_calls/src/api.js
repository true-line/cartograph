const API_ROUTE = {
  GET_USER: { method: "GET", path: "/api/users/:id" },
  CREATE_USER: { method: "POST", path: "/api/users" },
};

const callAPI = async function (route, body) {
  const { method, path } = route;
  return fetch(API_URL + path, { method, body });
};

export async function load() {
  return callAPI(API_ROUTE.GET_USER);
}

export async function create(user) {
  return callAPI(API_ROUTE.CREATE_USER, user);
}
