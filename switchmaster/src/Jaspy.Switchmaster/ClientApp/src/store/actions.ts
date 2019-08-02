import { ActionTree } from 'vuex';
import { Switch, SwitchStoreState } from './models';

export default {
    async fetch({ commit }) {
        try {
            commit('setProcessing', true);
            const response = await fetch('/api/switch');
            const items = await response.json();
            commit('setItems', items);
        } catch (err) {
            commit('addError', err);
        } finally {
            commit('setProcessing', false);
        }
    },
    async update({ commit }, payload: Switch) {
        try {
            commit('setProcessing', true);
            await fetch(`/api/switch/${payload.fqdn}`, {
                method: 'PATCH',
                headers: {
                    'Accept': 'application/json',
                    'Content-Type': 'application/json'
                },
                body: JSON.stringify(payload)
            });
            commit('updateItem', payload);
        } catch (err) {
            commit('addError', err);
        } finally {
            commit('setProcessing', false);
        }
    },
    async synchronize({ commit }) {
        try {
            commit('setProcessing', true);
            const response = await fetch('/api/switch/synchronize', {
                method: 'SYNCHRONIZE'
            });
            const result = await response.json();
        } catch (err) {
            commit('addError', err);
        } finally {
            commit('setProcessing', false);
        }
    }
} as ActionTree<SwitchStoreState, any>;
