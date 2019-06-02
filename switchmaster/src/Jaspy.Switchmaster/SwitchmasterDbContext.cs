using Jaspy.Switchmaster.Data.Entities;
using Microsoft.EntityFrameworkCore;

namespace Jaspy.Switchmaster.Data
{
    public class SwitchmasterDbContext : DbContext
    {
        public DbSet<Switch> Switches { get; set; }

        protected SwitchmasterDbContext()
        {
        }

        public SwitchmasterDbContext(DbContextOptions options) : base(options)
        {
        }
    }
}