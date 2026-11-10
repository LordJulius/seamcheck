async function loadPosts(page) {
  const res = await fetch(`/api/posts?page=${page}`);
  return res.json();
}

async function search(q) {
  const res = await fetch("/search?q=" + encodeURIComponent(q));
  return res.json();
}
