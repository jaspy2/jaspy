FROM mcr.microsoft.com/dotnet/core/sdk:2.2 AS builder
ARG Configuration=Release
ARG RuntimeIdentifier=linux-x64

RUN apt-get update -yq
RUN curl -sL https://deb.nodesource.com/setup_12.x | bash -
RUN apt-get update -yq \
  && apt-get install -yq \
    curl \
    git \
    nano\
    nodejs \
    build-essential \
   && rm -rf /var/lib/apt/lists/*
RUN npm install -g npm

WORKDIR /work

COPY ./*.sln .

RUN mkdir -p /work/src/Jaspy.Switchmaster
COPY ./src/Jaspy.Switchmaster/Jaspy.Switchmaster.csproj /work/src/Jaspy.Switchmaster

RUN dotnet restore switchmaster.sln

COPY . .

RUN dotnet publish --self-contained -r $RuntimeIdentifier -c $Configuration -o /app ./src/Jaspy.Switchmaster

CMD cp -r /app/* /output

FROM mcr.microsoft.com/dotnet/core/aspnet:latest

WORKDIR /app

COPY --from=builder /app .

ENTRYPOINT [ "./Jaspy.Switchmaster" ]
