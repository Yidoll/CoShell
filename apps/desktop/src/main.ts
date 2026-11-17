import { createPinia } from "pinia";
import { createApp } from "vue";
import StartupGate from "@/StartupGate.vue";
import "@/styles/global.css";
import "@/styles/workbench.css";

const app = createApp(StartupGate);
app.use(createPinia());
app.mount("#root");
