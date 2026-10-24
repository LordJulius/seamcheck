QString kind(QString t) {
  if (t.contains("head")) return "a";
  if (t.contains("vid")) return "b";
}
