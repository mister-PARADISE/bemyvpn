/* Выполняется до первой отрисовки: свёрнутая панель не «мигает» при загрузке. */
try {
  if (localStorage.getItem("perimeter.side") === "closed") document.documentElement.classList.add("collapsed");
} catch (e) { /* хранилище недоступно — панель просто открыта */ }
