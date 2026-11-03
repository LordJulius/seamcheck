(function chatAppBootstrap() {
  'use strict';

  const state = { currentChatId: null, showArchivedInbox: false };

  async function parseJsonResponse(response) {
    if (!response.ok) {
      throw new Error('HTTP ' + response.status);
    }
    return response.json();
  }

  async function requestJson(url, options) {
    const response = await fetch(url, options || {});
    return parseJsonResponse(response);
  }

  async function loadMessage(messageId) {
    const data = await requestJson(`/chat/messages/${messageId}`);
    return data;
  }

  async function poll(afterId) {
    const data = await requestJson(`/chat/${state.currentChatId}/messages?after_id=${afterId}`, { method: 'GET' });
    return data;
  }

  async function loadInbox() {
    const listUrl = state.showArchivedInbox ? '/chat/?include_archived=1' : '/chat/';
    const data = await requestJson(listUrl);
    return data;
  }

  function bind(root) {
    root.addEventListener('click', async (ev) => {
      const help = await requestJson('/chat/help-examples');
      const msg = await loadMessage(ev.target.dataset.id);
      const req = { body: ev.target.dataset.body };
      await requestJson(`/chat/${state.currentChatId}/send`, { method: 'POST', body: JSON.stringify(req.body) });
      return [help, msg, await poll(1), await loadInbox()];
    });
  }

  document.addEventListener('DOMContentLoaded', () => bind(document.body));
})();
