import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';
import { locale } from './lib/i18n';

document.documentElement.lang = locale;

export default mount(App, { target: document.getElementById('app')! });
