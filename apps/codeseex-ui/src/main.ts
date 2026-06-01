import App from "./App.svelte";
import "./styles.css";
import { mount } from "svelte";

const app = mount(App, {
  target: document.getElementById("app") as HTMLElement
});

export default app;
