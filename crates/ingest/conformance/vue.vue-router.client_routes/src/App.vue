<template><router-view /></template>
<script>
import VueRouter from "vue-router";
import Users from "./Users";
const router = new VueRouter({ routes: [{ path: "/users", component: Users }] });
export default { router };
</script>
