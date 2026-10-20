import { Route, Routes } from "react-router-dom";
function Users() { return <div />; }
export function App() { return <Routes><Route path="/users" element={<Users />} /></Routes>; }
