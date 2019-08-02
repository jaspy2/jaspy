using System;
using System.Collections.Generic;
using System.Data;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using Jaspy.Switchmaster.Data;
using McMaster.Extensions.CommandLineUtils;
using Microsoft.AspNetCore;
using Microsoft.AspNetCore.Hosting;
using Microsoft.EntityFrameworkCore;
using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;

namespace Jaspy.Switchmaster
{
    public class Program
    {
        public static void Main(string[] args)
        {
            var webHost = CreateWebHostBuilder(args).Build();
            var app = new CommandLineApplication
            {
                Name = "jaspy-switchmaster"
            };
            app.HelpOption("-h|--help", true);

            app.Command("reset", command =>
            {
                command.Description = "Reset switchmaster database";
                command.OnExecute(async () =>
                {
                    var serviceScope = webHost.Services.CreateScope();
                    using (var dbContext = serviceScope.ServiceProvider.GetRequiredService<SwitchmasterDbContext>())
                    {
                        var switches = await dbContext.Switches.ToListAsync();
                        dbContext.Switches.RemoveRange(switches);
                        await dbContext.SaveChangesAsync();
                    }
                });
            });

            app.Command("migrate", command =>
            {
                command.Description = "Apply database migrations";
                command.OnExecute(async () =>
                {
                    var serviceScope = webHost.Services.CreateScope();
                    using (var dbContext = serviceScope.ServiceProvider.GetRequiredService<SwitchmasterDbContext>())
                    {
                        await dbContext.Database.MigrateAsync();
                    }
                });
            });

            if (args.Length == 0)
            {
                webHost.Run();
            }
            else
            {
                try
                {
                    app.Execute(args);
                }
                catch (Exception e)
                {
                    Console.WriteLine("Failed to execute the command: {0}", e);
                }
            }
        }

        public static IWebHostBuilder CreateWebHostBuilder(string[] args) =>
            WebHost.CreateDefaultBuilder(args)
                .UseStartup<Startup>();
    }
}
