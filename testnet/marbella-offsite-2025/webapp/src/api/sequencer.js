const BASE_URL = import.meta.env.VITE_API_BASE_URL || 'http://localhost:8080';

export const sequencerApi = {
  async getAccount(accountName) {
    const res = await fetch(`${BASE_URL}/accounts/${accountName}?tx=true`);
    if (!res.ok) throw new Error('Account not found');
    return res.json();
  },

  async postTransfer(from, to, amount) {
    const res = await fetch(`${BASE_URL}/transfer`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ from, to, amount }),
    });
    if (!res.ok) {
      const error = await res.json();
      throw new Error(error.error || 'Transfer failed');
    }
    return res.json();
  }
};
