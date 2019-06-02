using Jaspy.Switchmaster.Data.Models;

namespace Jaspy.Switchmaster.Data.Entities
{
    public class Switch
    {
        public long Id { get; set; }
        public string Fqdn { get; set; }
        public bool Configured { get; set; }
        public DeployState DeployState { get; set; }
    }
}