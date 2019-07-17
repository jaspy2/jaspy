import { ActionTree } from 'vuex';
import { SwitchStoreState } from './models';

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
    async synchronize({ commit }) {
        try {
            commit('setProcessing', true);
            const response = await fetch('/api/switch/synchronize', {
                method: 'SYNCHRONIZE',
            });
            const result = await response.json();
            commit('setSyncResult', result);
        } catch (err) {
            commit('addError', err);
        } finally {
            commit('setProcessing', false);
        }
    },
} as ActionTree<SwitchStoreState, any>;
