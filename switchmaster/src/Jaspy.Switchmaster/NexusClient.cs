using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Threading.Tasks;
using Jaspy.Switchmaster.Data.Models;
using Newtonsoft.Json;

namespace Jaspy.Switchmaster
{
    public class NexusDevice
    {
        public int Id { get; set; }
        public string Name { get; set; }
        public string DnsDomain { get; set; }
        public string SnmpCommunity { get; set; }
        public string BaseMac { get; set; }
        public bool PollingEnabled { get; set; }
        public string OsInfo { get; set; }
    }
    
    public class NexusClient
    {
        private readonly string _apiRoot;

        public NexusClient(string apiRoot)
        {
            _apiRoot = apiRoot;
        }

        public async Task<IEnumerable<NexusDevice>> ListDevicesAsync()
        {
            var endpointUrl = $"{_apiRoot}/device";
            using (var client = new HttpClient())
            {
                var response = await client.GetStringAsync(endpointUrl);
                return JsonConvert.DeserializeObject<IEnumerable<NexusDevice>>(response);
            }
        }
    }
}