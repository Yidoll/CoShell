import { createPinia } from "pinia";
import { createApp } from "vue";
import StartupGate from "@/StartupGate.vue";
import { initializeTheme } from "@/lib/theme";
import "@/styles/global.css";
import "@/styles/workbench.css";

const stopTheme = initializeTheme();
import.meta.hot?.dispose(stopTheme);
const app = createApp(StartupGate);
app.use(createPinia());
app.mount("#root");
