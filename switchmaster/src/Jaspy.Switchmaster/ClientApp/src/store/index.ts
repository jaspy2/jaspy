import Vue from 'vue';
import Vuex from 'vuex';
import switchModule from './modules/switch';

Vue.use(Vuex);

export default new Vuex.Store({
    modules: {
        switch: switchModule,
    },
    strict: true,
});
